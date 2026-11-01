//! The GTK user interface and the glue between session, network and media.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use twofcast_core::annotations::{
    ARROW_MAX_WIDTH, ARROW_MIN_WIDTH, ARROW_WIDTH, AnnotOp, IdGen, Item, PALETTE, Pt, Rgb,
    STROKE_WIDTH, Shape, TEXT_SIZE,
};
use twofcast_core::config::{self, Config};
use twofcast_core::control;
use twofcast_core::deps;
use twofcast_core::layout::{CANVAS_H, CANVAS_W, Fit, Layout, PadParams, Preset, Rect, TileId};
use twofcast_core::levels;
use twofcast_core::mic_channel::Mode as MicMode;
use twofcast_core::net::{Net, NetConfig, NetEvent};
use twofcast_core::pipelines::{Channels, Ports, ScreenSource, TrackNames};
use twofcast_core::protocol::Msg;
use twofcast_core::pulse::Target;
use twofcast_core::session::{Event, Out, Session};
use twofcast_core::{gate, voice};

use crate::media::{Media, MediaConfig, camera_source, mic_source};
use crate::share_audio::{self, ShareAudio};
use crate::windows;

const APP_ID: &str = "org.twofcast.TwoFCast";

const METER_HEIGHT: i32 = 46;
const ZONE_GREEN: (f64, f64, f64) = (0.18, 0.76, 0.38);
const ZONE_YELLOW: (f64, f64, f64) = (0.96, 0.78, 0.17);
const ZONE_RED: (f64, f64, f64) = (0.88, 0.2, 0.2);

fn zone_color(z: levels::Zone) -> (f64, f64, f64) {
    match z {
        levels::Zone::Green => ZONE_GREEN,
        levels::Zone::Yellow => ZONE_YELLOW,
        levels::Zone::Red => ZONE_RED,
    }
}

/// Shortens `text` with "…" to fit `max_w` at the current font.
fn ellipsize(cr: &gtk::cairo::Context, text: &str, max_w: f64) -> String {
    let fits = |t: &str| {
        cr.text_extents(t)
            .map(|e| e.x_advance() <= max_w)
            .unwrap_or(true)
    };
    if fits(text) {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let t: String = chars.iter().collect::<String>() + "…";
        if fits(&t) {
            return t;
        }
    }
    String::new()
}

const CSS: &str = "
.swatch { min-width: 22px; min-height: 22px; padding: 0; border-radius: 11px; }
.swatch:checked { outline: 3px solid @theme_fg_color; outline-offset: 1px; }
.swatch-0 { background: rgb(235,64,52); }
.swatch-1 { background: rgb(250,204,21); }
.swatch-2 { background: rgb(34,197,94); }
.swatch-3 { background: rgb(59,130,246); }
.recording { background: #c01c28; color: white; }
.preview { background: #111; }
.error-bar { color: #e01b24; padding: 2px 8px; }
.recording-status { color: #e01b24; font-weight: bold; }
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Ping,
    Pen,
    Arrow,
    Line,
    Rect,
    Text,
    Arrange,
}

impl Tool {
    /// The shape this tool drags out, if it is a shape tool.
    fn shape(self) -> Option<Shape> {
        match self {
            Tool::Arrow => Some(Shape::Arrow),
            Tool::Line => Some(Shape::Line),
            Tool::Rect => Some(Shape::Rect),
            Tool::Ping | Tool::Pen | Tool::Text | Tool::Arrange => None,
        }
    }
}

struct Ui {
    window: gtk::ApplicationWindow,
    status: gtk::Label,
    error: gtk::Label,
    /// Connection problems (e.g. a firewall); hidden when all is well.
    warning: gtk::Label,
    share_list: gtk::Box,
    share_popover: gtk::Popover,
    stop_share: gtk::Button,
    record: gtk::Button,
    picture: gtk::Picture,
    area: gtk::DrawingArea,
    text_popover: gtk::Popover,
    text_view: gtk::TextView,
    meters: gtk::DrawingArea,
    /// "Artem recording 00:12", one line per machine that records.
    recording_status: gtk::Label,
    /// Color buttons, in [`PALETTE`] order.
    swatches: Vec<gtk::ToggleButton>,
}

pub struct App {
    cfg: Config,
    cfg_dir: PathBuf,
    session: Session,
    media: Media,
    net: Net,
    share_audio: Option<ShareAudio>,
    /// Size of the shared window before it was shrunk for the recording.
    shared_window: Option<windows::Saved>,
    ids: IdGen,
    tool: Tool,
    color: Rgb,
    stroke: Option<u64>,
    /// Arrow (line, rectangle) being drawn: id and current tip.
    arrow: Option<(u64, Pt)>,
    arrow_width: f32,
    /// The current press only unmuted the mic; it draws nothing.
    swallow_gesture: bool,
    drag_start: (f64, f64),
    drag_tile: Option<(TileId, Rect)>,
    pointer: (f64, f64),
    my_items: Vec<u64>,
    text_at: Option<Pt>,
    /// Text being typed: its id once it exists (shown to both of us).
    typing: Option<u64>,
    /// The user chose a color; otherwise it follows `my_default_color`.
    color_picked: bool,
    frame_serial: u64,
    /// File being recorded on this machine.
    recording_file: Option<PathBuf>,
    /// Button choices remembered across restarts (e.g. hidden self-view).
    prefs: config::UiPrefs,
    /// What I shared last, to share it again when the partner, who took the
    /// shared space over, stops sharing.
    last_share: Option<(ScreenSource, Option<Target>)>,
    /// What we last told the peer about receiving their media.
    reported_receiving: Option<bool>,
    ui: Ui,
}

type Shared = Rc<RefCell<App>>;

/// Runs `f` on the app state.  GTK may emit a signal while another handler
/// still runs (opening a popover cancels the drag gesture, for example); such
/// nested calls are deferred to the next idle moment instead of panicking.
fn with(app: &Shared, f: impl FnOnce(&mut App) + 'static) {
    match app.try_borrow_mut() {
        Ok(mut a) => f(&mut a),
        Err(_) => {
            let app = app.clone();
            glib::idle_add_local_once(move || f(&mut app.borrow_mut()));
        }
    }
}

/// Diagnostics on stderr when `TWOFCAST_DEBUG` is set.
fn debug(msg: &str) {
    crate::log::write(msg);
}

pub fn run() -> glib::ExitCode {
    let mut cfg_dir = config::config_dir();
    let mut control_path: Option<PathBuf> = std::env::var_os("TWOFCAST_CONTROL").map(PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config-dir" => cfg_dir = args.next().map(PathBuf::from).unwrap_or(cfg_dir),
            "--control" => control_path = args.next().map(PathBuf::from),
            "--self-test" => return crate::selftest::run(),
            "--ctl" => {
                // Client mode: send one command to a running instance.
                let (Some(path), Some(cmd)) = (args.next(), args.next()) else {
                    eprintln!("usage: 2fcast --ctl SOCKET '{{\"cmd\":\"state\"}}'");
                    return glib::ExitCode::FAILURE;
                };
                return match crate::control::request(path.as_ref(), &cmd) {
                    Ok(reply) => {
                        print!("{reply}");
                        let ok = serde_json::from_str::<control::Reply>(&reply).is_ok_and(|r| r.ok);
                        if ok {
                            glib::ExitCode::SUCCESS
                        } else {
                            glib::ExitCode::FAILURE
                        }
                    }
                    Err(e) => {
                        eprintln!("{e:#}");
                        glib::ExitCode::FAILURE
                    }
                };
            }
            "-h" | "--help" => {
                println!(
                    "2fcast - two people, one shared screen, one recording.\n\n\
                     Usage: 2fcast [--config-dir DIR] [--control SOCKET]\n       \
                     2fcast --ctl SOCKET JSON   (send a command to a running 2fcast)\n       \
                     2fcast --self-test         (check that this installation works)\n\n\
                     Settings: {}/config.toml",
                    cfg_dir.display()
                );
                return glib::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return glib::ExitCode::FAILURE;
            }
        }
    }
    crate::log::init();
    if let Err(e) = gst::init() {
        eprintln!("GStreamer is not available: {e}");
        return glib::ExitCode::FAILURE;
    }
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |gapp| {
        load_css();
        let missing =
            deps::missing_packages(|e| gst::ElementFactory::find(e).is_some(), deps::in_path);
        if !missing.is_empty() {
            show_install_window(gapp, &missing);
            return;
        }
        if let Err(e) = build(gapp, cfg_dir.clone(), control_path.clone()) {
            show_fatal(gapp, &format!("{e:#}"));
        }
    });
    app.run_with_args(&["2fcast"])
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    #[allow(deprecated)]
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        #[allow(deprecated)]
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn show_fatal(gapp: &gtk::Application, text: &str) {
    eprintln!("[2fcast] {text}");
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    gtk::ApplicationWindow::builder()
        .application(gapp)
        .title("2fcast - error")
        .child(&label)
        .build()
        .present();
}

/// Offers to install missing Ubuntu packages, then restarts the app.
fn show_install_window(gapp: &gtk::Application, missing: &[&str]) {
    let cmd = deps::install_command(missing);
    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    let text = format!(
        "2fcast needs a few more system packages:\n\n    {}\n\nInstall them now? You will be asked for your password.",
        missing.join("\n    ")
    );
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .selectable(true)
        .build();
    let cmd_label = gtk::Label::builder()
        .label(format!(
            "Or in a terminal:  {}",
            deps::terminal_command(missing)
        ))
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let button = gtk::Button::with_label("Install");
    button.add_css_class("suggested-action");
    let result = gtk::Label::builder()
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .build();
    // apt's own output, shown when something goes wrong.
    let log = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .selectable(true)
        .css_classes(["monospace", "dim-label"])
        .build();
    let log_view = gtk::ScrolledWindow::builder()
        .child(&log)
        .min_content_height(160)
        .min_content_width(640)
        .visible(false)
        .build();
    vbox.append(&label);
    vbox.append(&cmd_label);
    vbox.append(&button);
    vbox.append(&result);
    vbox.append(&log_view);
    let window = gtk::ApplicationWindow::builder()
        .application(gapp)
        .title("2fcast - setup")
        .child(&vbox)
        .build();
    button.connect_clicked(move |b| {
        b.set_sensitive(false);
        result.set_text("Installing…");
        let (tx, rx) = async_channel::bounded(1);
        let cmd = cmd.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new(&cmd[0]).args(&cmd[1..]).output();
            let reply = match out {
                Ok(o) => (
                    o.status.success(),
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                ),
                Err(e) => (false, format!("cannot run {}: {e}", cmd[0])),
            };
            let _ = tx.send_blocking(reply);
        });
        let result = result.clone();
        let (log, log_view) = (log.clone(), log_view.clone());
        let b = b.clone();
        glib::spawn_future_local(async move {
            let (ok, output) = rx.recv().await.unwrap_or((false, String::new()));
            if ok {
                use std::os::unix::process::CommandExt;
                // Restart so GStreamer sees the new plugins.
                let exe = std::env::current_exe().unwrap_or_else(|_| "2fcast".into());
                let err = std::process::Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .exec();
                result.set_text(&format!("Installed. Please start 2fcast again ({err})."));
            } else {
                result.set_text(&deps::explain_apt_failure(&output));
                let tail: Vec<&str> = output.lines().rev().take(25).collect();
                log.set_text(&tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
                log_view.set_visible(!output.trim().is_empty());
                b.set_sensitive(true);
            }
        });
    });
    window.present();
}

fn build(
    gapp: &gtk::Application,
    cfg_dir: PathBuf,
    control_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let cfg = Config::load_or_create(&cfg_dir)?;
    crate::log::write(&format!(
        "GStreamer {}, H.264 decoder {}; name {:?}, camera {:?}, mic {:?} ({}), gate {}, high-pass {} Hz, compressor {}, duck {} dB, ports {}, discovery {}, peer_address {:?}",
        gst::version_string(),
        crate::media::h264_decoder(),
        cfg.name,
        cfg.camera,
        cfg.microphone,
        cfg.microphone_channel,
        if cfg.noise_gate {
            format!("{} dB", cfg.noise_gate_db)
        } else {
            "off".into()
        },
        cfg.high_pass_hz,
        cfg.compressor,
        cfg.duck_partner_db,
        cfg.base_port,
        cfg.discovery,
        cfg.peer_address,
    ));
    share_audio::cleanup_stale();

    // Errors from GStreamer threads are shown in the window.
    let (err_tx, err_rx) = async_channel::unbounded::<String>();
    let on_error: crate::media::ErrorFn = Arc::new(move |e| {
        let _ = err_tx.send_blocking(e);
    });
    let ports = Ports::from_base(cfg.base_port);
    let media = Media::new(
        MediaConfig {
            channels: Channels::new(""),
            ports,
            camera: camera_source(&cfg.camera),
            mic: mic_source(&cfg.microphone),
            mic_channel: MicMode::parse(&cfg.microphone_channel),
            noise_gate_db: gate::threshold_from_setting(cfg.noise_gate, cfg.noise_gate_db),
            high_pass_hz: voice::high_pass_from_setting(cfg.high_pass_hz),
            compressor: cfg.compressor,
            duck_db: voice::duck_from_setting(cfg.duck_partner_db),
            playback: true,
            h264_decoder: None,
        },
        on_error,
    )?;

    let (net_tx, net_rx) = async_channel::unbounded::<NetEvent>();
    let static_peer = cfg.peer_address.as_deref().and_then(|a| a.parse().ok());
    let net = Net::start(
        NetConfig {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            port: ports.control,
            base_port: cfg.base_port,
            discovery: cfg.discovery,
            static_peer,
        },
        move |ev| {
            let _ = net_tx.send_blocking(ev);
        },
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "cannot listen on port {}: {e}. Is 2fcast already running?",
            ports.control
        )
    })?;

    let layout = config::load_layout(&cfg_dir)
        .unwrap_or_else(|| Layout::preset(Preset::ScreenCamsRight, std::slice::from_ref(&cfg.id)));
    let session = Session::new(cfg.id.clone(), cfg.name.clone(), layout);

    let ui = build_widgets(gapp, &cfg, &cfg_dir);
    let prefs = config::load_ui_prefs(&cfg_dir);
    let app: Shared = Rc::new(RefCell::new(App {
        cfg,
        cfg_dir,
        session,
        media,
        net,
        share_audio: None,
        shared_window: None,
        ids: IdGen::new(),
        tool: Tool::Ping,
        color: PALETTE[0],
        stroke: None,
        arrow: None,
        arrow_width: ARROW_WIDTH,
        swallow_gesture: false,
        drag_start: (0.0, 0.0),
        drag_tile: None,
        pointer: (0.0, 0.0),
        my_items: vec![],
        text_at: None,
        typing: None,
        color_picked: false,
        frame_serial: 0,
        recording_file: None,
        prefs,
        last_share: None,
        reported_receiving: None,
        ui,
    }));
    connect_signals(&app);
    app.borrow_mut().apply(Out::default());

    let a = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(ev) = net_rx.recv().await {
            with(&a, move |app| app.on_net(ev));
        }
    });
    let a = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(e) = err_rx.recv().await {
            with(&a, move |app| app.ui.error.set_text(&e));
        }
    });
    if let Some(path) = control_path {
        let (ctl_tx, ctl_rx) = async_channel::unbounded::<crate::control::Request>();
        crate::control::serve(&path, ctl_tx)?;
        eprintln!("[2fcast] control API on {}", path.display());
        let a = app.clone();
        glib::spawn_future_local(async move {
            while let Ok((cmd, reply)) = ctl_rx.recv().await {
                let shared = a.clone();
                with(&a, move |app| {
                    let _ = reply.send(app.handle_control(cmd, &shared));
                });
            }
        });
    }
    let a = app.clone();
    glib::timeout_add_local(Duration::from_millis(1000), move || {
        if let Ok(mut app) = a.try_borrow_mut() {
            if let Some(s) = &app.share_audio {
                s.poll();
            }
            app.check_media_arrives();
            app.refresh_ui();
        }
        glib::ControlFlow::Continue
    });
    app.borrow().ui.window.present();
    Ok(())
}

fn build_widgets(gapp: &gtk::Application, cfg: &Config, cfg_dir: &std::path::Path) -> Ui {
    let window = gtk::ApplicationWindow::builder()
        .application(gapp)
        .title("2fcast")
        .default_width(1280)
        .default_height(820)
        .build();
    let header = gtk::HeaderBar::new();
    let status = gtk::Label::new(Some("Starting…"));
    header.set_title_widget(Some(&status));

    let share_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&share_list)
        .min_content_height(300)
        .min_content_width(380)
        .propagate_natural_height(true)
        .build();
    let share_popover = gtk::Popover::builder().child(&scroller).build();
    let share_button = gtk::MenuButton::builder()
        .label("Share…")
        .popover(&share_popover)
        .tooltip_text("Show one of your windows in the shared space")
        .build();
    share_button.add_css_class("suggested-action");
    let stop_share = gtk::Button::with_label("Stop sharing");
    header.pack_start(&share_button);
    header.pack_start(&stop_share);

    let record = gtk::Button::with_label("● Record");
    record.set_widget_name("record");
    let mic = gtk::ToggleButton::with_label("Mic on");
    mic.set_widget_name("mic");
    let self_view = gtk::ToggleButton::builder()
        .label("Self-view on")
        .tooltip_text(
            "Hide your own camera on your screen, so seeing yourself doesn't distract you. \
             Your partner still sees you and the recording still includes you.",
        )
        .build();
    self_view.set_widget_name("self-view");
    let layout_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    layout_box.set_widget_name("layout-box");
    let layout_button = gtk::MenuButton::builder()
        .label("Layout")
        .popover(&gtk::Popover::builder().child(&layout_box).build())
        .build();
    // The Arrange tool button goes here (next to Layout), see `connect_signals`.
    let arrange_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    arrange_slot.set_widget_name("arrange-slot");
    header.pack_end(&record);
    header.pack_end(&mic);
    header.pack_end(&self_view);
    header.pack_end(&layout_button);
    header.pack_end(&arrange_slot);
    window.set_titlebar(Some(&header));

    let tools = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(8)
        .margin_end(8)
        .build();
    tools.set_widget_name("tools");

    let picture = gtk::Picture::new();
    picture.add_css_class("preview");
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&area);

    // Multi-line, for code: the partner sees every keystroke.  Escape,
    // Ctrl+Enter or a click elsewhere finishes.
    let text_view = gtk::TextView::builder()
        .monospace(true)
        .accepts_tab(true)
        .wrap_mode(gtk::WrapMode::None)
        .build();
    let text_scroller = gtk::ScrolledWindow::builder()
        .child(&text_view)
        .min_content_width(380)
        .min_content_height(60)
        .max_content_height(300)
        .propagate_natural_height(true)
        .build();
    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    text_box.append(&text_scroller);
    let text_hint = gtk::Label::new(Some("Live for both of you · Ctrl+Enter or Esc: done"));
    text_hint.add_css_class("dim-label");
    text_hint.set_xalign(0.0);
    text_box.append(&text_hint);
    let text_popover = gtk::Popover::builder()
        .child(&text_box)
        .position(gtk::PositionType::Top)
        .build();
    text_popover.set_parent(&area);

    let error = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    error.add_css_class("error-bar");
    // Clickable paths: open the folder (created if needed) in the file manager.
    let folder_link = |title: &str, tooltip: &str, path: &std::path::Path, folder: PathBuf| {
        let text = glib::markup_escape_text(&path.display().to_string());
        let label = gtk::Label::builder()
            .use_markup(true)
            .label(format!("{title}: <a href=\"open\">{text}</a>"))
            .xalign(1.0)
            .tooltip_text(tooltip)
            .build();
        label.connect_activate_link(move |_, _| {
            let _ = std::fs::create_dir_all(&folder);
            if let Err(e) = std::process::Command::new("xdg-open").arg(&folder).spawn() {
                crate::log::write(&format!("cannot open {}: {e}", folder.display()));
            }
            glib::Propagation::Stop
        });
        label
    };
    let log_path = crate::log::path();
    let log_dir = log_path.parent().map(PathBuf::from).unwrap_or_default();
    let folder_links = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .margin_end(8)
        .css_classes(["dim-label"])
        .build();
    folder_links.append(&folder_link(
        "Recordings",
        "Open the recordings folder",
        &cfg.output_dir,
        cfg.output_dir.clone(),
    ));
    folder_links.append(&folder_link(
        "Log",
        "Open the folder with the log",
        &log_path,
        log_dir,
    ));
    let meters = gtk::DrawingArea::builder()
        .content_width(470)
        .content_height(METER_HEIGHT)
        .margin_start(8)
        .margin_top(2)
        .margin_bottom(2)
        .tooltip_text(format!(
            "You are {}, you can change this name in {}\n\n\
             Voice levels in dBFS. Speech should stay in the green; red will distort. \
             The dashed blue mark is the noise gate: quieter sound is muted.",
            cfg.name,
            cfg_dir.join("config.toml").display()
        ))
        .build();
    let recording_status = gtk::Label::builder()
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .visible(false)
        .css_classes(["recording-status"])
        .build();
    error.set_hexpand(true);
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bottom.append(&meters);
    bottom.append(&recording_status);
    bottom.append(&error);
    bottom.append(&folder_links);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let warning = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .visible(false)
        .margin_start(8)
        .build();
    warning.add_css_class("error-bar");
    vbox.append(&tools);
    vbox.append(&warning);
    vbox.append(&overlay);
    vbox.append(&bottom);
    window.set_child(Some(&vbox));

    Ui {
        window,
        status,
        error,
        warning,
        share_list,
        share_popover,
        stop_share,
        record,
        picture,
        area,
        text_popover,
        text_view,
        meters,
        recording_status,
        swatches: vec![],
    }
}

fn find<W: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>, name: &str) -> Option<W> {
    let mut stack = vec![root.clone().upcast::<gtk::Widget>()];
    if let Some(p) = root
        .dynamic_cast_ref::<gtk::MenuButton>()
        .and_then(|m| m.popover())
    {
        stack.push(p.upcast());
    }
    while let Some(w) = stack.pop() {
        if w.widget_name() == name {
            return w.downcast().ok();
        }
        if let Some(m) = w.downcast_ref::<gtk::MenuButton>()
            && let Some(p) = m.popover()
        {
            stack.push(p.upcast());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            stack.push(child);
        }
    }
    None
}

fn connect_signals(app: &Shared) {
    let (window, area, tools) = {
        let a = app.borrow();
        let tools: gtk::Box = find(&a.ui.window, "tools").unwrap();
        (a.ui.window.clone(), a.ui.area.clone(), tools)
    };

    // --- tools row ---
    let mut first: Option<gtk::ToggleButton> = None;
    // Tool buttons that have a shortcut, to press them from the keyboard.
    let mut keyed: Vec<(&str, gtk::ToggleButton)> = Vec::new();
    for (tool, label, tip, key) in [
        (
            Tool::Ping,
            "◎ Ping",
            "Click to show the partner where to look (Ctrl+P); right click pings with any tool",
            Some("<Control>p"),
        ),
        (Tool::Pen, "✎ Pen", "Draw over the shared space", None),
        (
            Tool::Arrow,
            "➜ Arrow",
            "Drag to draw an arrow; the mouse wheel changes its width (Ctrl+A)",
            Some("<Control>a"),
        ),
        (
            Tool::Line,
            "╱ Line",
            "Drag to draw a line; the mouse wheel changes its width (Ctrl+L)",
            Some("<Control>l"),
        ),
        (
            Tool::Rect,
            "▭ Rectangle",
            "Drag to draw a rectangle; the mouse wheel changes its width (Ctrl+R)",
            Some("<Control>r"),
        ),
        (
            Tool::Text,
            "T Text",
            "Click on the shared space to write; the partner sees you type (Ctrl+T)",
            Some("<Control>t"),
        ),
        (
            Tool::Arrange,
            "⬚ Arrange",
            "Drag tiles to move them, scroll to resize",
            None,
        ),
    ] {
        let b = gtk::ToggleButton::builder()
            .label(label)
            .tooltip_text(tip)
            .build();
        b.set_group(first.as_ref());
        if first.is_none() {
            b.set_active(true);
            first = Some(b.clone());
        }
        let a = app.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                with(&a, move |app| {
                    app.tool = tool;
                    app.ui.area.queue_draw();
                });
            }
        });
        if let Some(key) = key {
            keyed.push((key, b.clone()));
        }
        if tool == Tool::Arrange {
            find::<gtk::Box>(&window, "arrange-slot")
                .unwrap()
                .append(&b);
        } else {
            tools.append(&b);
        }
    }
    tools.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    let mut first: Option<gtk::ToggleButton> = None;
    let mut swatches = vec![];
    for (i, color) in PALETTE.iter().enumerate() {
        let b = gtk::ToggleButton::new();
        b.add_css_class("swatch");
        b.add_css_class(&format!("swatch-{i}"));
        b.set_valign(gtk::Align::Center);
        b.set_group(first.as_ref());
        if first.is_none() {
            b.set_active(true);
            first = Some(b.clone());
        }
        let a = app.clone();
        let color = *color;
        b.connect_toggled(move |b| {
            if b.is_active() {
                with(&a, move |app| {
                    app.color = color;
                    // Picking the default (or us setting it) keeps the default.
                    app.color_picked = color != app.session.my_default_color();
                });
            }
        });
        tools.append(&b);
        swatches.push(b);
    }
    app.borrow_mut().ui.swatches = swatches;
    tools.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    let undo = gtk::Button::with_label("Undo");
    let a = app.clone();
    undo.connect_clicked(move |_| with(&a, |app| app.undo()));
    undo.set_tooltip_text(Some("Undo your last drawing (Ctrl+Z)"));
    // Ctrl+Z anywhere in the window.  Bubble phase: a focused text field
    // gets the key first and undoes its own typing.
    let shortcuts = gtk::ShortcutController::new();
    let a = app.clone();
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>z"),
        Some(gtk::CallbackAction::new(move |_, _| {
            with(&a, |app| app.undo());
            glib::Propagation::Stop
        })),
    ));
    // Ctrl+1 ... Ctrl+4: layout presets.
    for (i, p) in Preset::ALL.into_iter().enumerate() {
        let a = app.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(&format!("<Control>{}", i + 1)),
            Some(gtk::CallbackAction::new(move |_, _| {
                with(&a, move |app| app.apply_preset(p));
                glib::Propagation::Stop
            })),
        ));
    }
    // Ctrl+P / A / L / R / T: tools.  A focused text field gets the key first
    // (Ctrl+A there selects all).
    for (key, b) in keyed {
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(key),
            Some(gtk::CallbackAction::new(move |_, _| {
                b.set_active(true);
                glib::Propagation::Stop
            })),
        ));
    }
    // Ctrl+M: mute / unmute.
    let a = app.clone();
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>m"),
        Some(gtk::CallbackAction::new(move |_, _| {
            with(&a, |app| {
                if app.media.has_mic() {
                    let muted = app.media.is_mic_muted();
                    app.set_muted(!muted);
                }
            });
            glib::Propagation::Stop
        })),
    ));
    // Ctrl+C: clear drawings (a focused text field copies instead).
    let a = app.clone();
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>c"),
        Some(gtk::CallbackAction::new(move |_, _| {
            with(&a, |app| app.clear_drawings());
            glib::Propagation::Stop
        })),
    ));
    window.add_controller(shortcuts);
    tools.append(&undo);
    let clear = gtk::Button::with_label("Clear drawings");
    clear.set_tooltip_text(Some("Remove all drawings (Ctrl+C)"));
    let a = app.clone();
    clear.connect_clicked(move |_| with(&a, |app| app.clear_drawings()));
    tools.append(&clear);

    // --- header ---
    let (share_popover, stop_share, record, mic, layout_box) = {
        let a = app.borrow();
        (
            a.ui.share_popover.clone(),
            a.ui.stop_share.clone(),
            a.ui.record.clone(),
            find::<gtk::ToggleButton>(&window, "mic").unwrap(),
            find::<gtk::Box>(&window, "layout-box").unwrap(),
        )
    };
    let a = app.clone();
    share_popover.connect_show(move |_| {
        let shared = a.clone();
        with(&a, move |app| app.fill_share_list(&shared));
    });
    let a = app.clone();
    stop_share.connect_clicked(move |_| with(&a, |app| app.stop_sharing()));
    let a = app.clone();
    record.connect_clicked(move |_| {
        let shared = a.clone();
        with(&a, move |app| {
            if app.media.is_recording() {
                app.confirm_stop_recording(&shared);
            } else {
                app.toggle_recording(&shared);
            }
        });
    });
    if !app.borrow().media.has_mic() {
        mic.set_label("No mic");
        mic.set_sensitive(false);
    }
    let self_view = find::<gtk::ToggleButton>(&window, "self-view").unwrap();
    if !app.borrow().media.has_camera() {
        self_view.set_visible(false);
    }
    let a = app.clone();
    self_view.connect_toggled(move |b| {
        let hidden = b.is_active();
        with(&a, move |app| app.set_self_view_hidden(hidden));
    });
    let hidden = app.borrow().prefs.hide_self_view;
    app.borrow().sync_self_view_button(hidden);
    let a = app.clone();
    mic.connect_toggled(move |b| {
        let muted = b.is_active();
        with(&a, move |app| app.media.set_mic_muted(muted));
        b.set_label(if b.is_active() { "Mic muted" } else { "Mic on" });
    });
    for (i, p) in Preset::ALL.into_iter().enumerate() {
        let b = gtk::Button::with_label(&format!("Ctrl+{}   {}", i + 1, p.label()));
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let a = app.clone();
        b.connect_clicked(move |_| with(&a, move |app| app.apply_preset(p)));
        layout_box.append(&b);
    }
    layout_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    for (label, which) in [
        ("Show / hide shared space", 0),
        ("Show / hide my camera", 1),
        ("Show / hide partner's camera", 2),
    ] {
        let b = gtk::Button::with_label(label);
        let a = app.clone();
        b.connect_clicked(move |_| with(&a, move |app| app.toggle_tile(which)));
        layout_box.append(&b);
    }
    let hint = gtk::Label::new(Some("Use the Arrange tool to drag and resize tiles."));
    hint.add_css_class("dim-label");
    layout_box.append(&hint);

    // --- preview & drawing ---
    let a = app.clone();
    app.borrow()
        .ui
        .picture
        .add_tick_callback(move |picture, _| {
            if let Ok(mut app) = a.try_borrow_mut()
                && let Some(f) = app.media.latest_frame()
                && f.serial != app.frame_serial
            {
                app.frame_serial = f.serial;
                let tex = gdk::MemoryTexture::new(
                    f.width,
                    f.height,
                    gdk::MemoryFormat::B8g8r8a8,
                    &f.data,
                    f.stride,
                );
                picture.set_paintable(Some(&tex));
            }
            glib::ControlFlow::Continue
        });
    let a = app.clone();
    area.set_draw_func(move |_, cr, w, h| {
        if let Ok(app) = a.try_borrow() {
            app.draw_hints(cr, w as f64, h as f64);
        }
    });

    let meters = app.borrow().ui.meters.clone();
    let a = app.clone();
    meters.set_draw_func(move |_, cr, w, _| {
        if let Ok(app) = a.try_borrow() {
            app.draw_meters(cr, w as f64);
        }
    });
    glib::timeout_add_local(Duration::from_millis(50), move || {
        meters.queue_draw();
        glib::ControlFlow::Continue
    });

    let drag = gtk::GestureDrag::new();
    let a = app.clone();
    drag.connect_drag_begin(move |_, x, y| with(&a, move |app| app.drag_begin(x, y)));
    let a = app.clone();
    drag.connect_drag_update(move |_, dx, dy| with(&a, move |app| app.drag_update(dx, dy)));
    let a = app.clone();
    drag.connect_drag_end(move |_, _, _| {
        with(&a, |app| {
            app.stroke = None;
            app.arrow = None;
            app.drag_tile = None;
        })
    });
    area.add_controller(drag);

    let click = gtk::GestureClick::new();
    let a = app.clone();
    click.connect_released(move |_, _, x, y| with(&a, move |app| app.click(x, y)));
    area.add_controller(click);
    // Right click: ping, whatever the tool.
    let right = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let a = app.clone();
    right.connect_pressed(move |_, _, x, y| {
        with(&a, move |app| {
            if let Some(at) = app.screen_point(x, y) {
                app.ping(at);
            }
        })
    });
    area.add_controller(right);

    let motion = gtk::EventControllerMotion::new();
    let a = app.clone();
    motion.connect_motion(move |_, x, y| with(&a, move |app| app.pointer = (x, y)));
    area.add_controller(motion);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let a = app.clone();
    scroll.connect_scroll(move |_, _, dy| {
        with(&a, move |app| app.scroll(dy));
        glib::Propagation::Stop
    });
    area.add_controller(scroll);

    let (text_view, text_popover) = {
        let a = app.borrow();
        (a.ui.text_view.clone(), a.ui.text_popover.clone())
    };
    let a = app.clone();
    text_view.buffer().connect_changed(move |b| {
        let text = b.text(&b.start_iter(), &b.end_iter(), false).to_string();
        with(&a, move |app| app.typed(text));
    });
    let keys = gtk::EventControllerKey::new();
    let popover = text_popover.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter);
        if enter && mods.contains(gdk::ModifierType::CONTROL_MASK) {
            popover.popdown();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    text_view.add_controller(keys);
    let a = app.clone();
    text_popover.connect_closed(move |_| with(&a, |app| app.finish_text()));

    let a = app.clone();
    window.connect_close_request(move |_| {
        with(&a, |app| app.shutdown());
        glib::Propagation::Proceed
    });
}

impl App {
    fn on_net(&mut self, ev: NetEvent) {
        let out = match ev {
            NetEvent::Connected(peer) => {
                debug(&format!(
                    "connected to {:?} at {} (base port {}, id {})",
                    peer.name, peer.ip, peer.base_port, peer.id
                ));
                self.reported_receiving = None;
                let out = self.session.connected(peer);
                if !self.color_picked {
                    let i = PALETTE
                        .iter()
                        .position(|&c| c == self.session.my_default_color());
                    if let Some(b) = i.and_then(|i| self.ui.swatches.get(i)) {
                        b.set_active(true);
                    }
                    self.color = self.session.my_default_color();
                }
                out
            }
            NetEvent::Message(m) => {
                if let Msg::Presenter { .. } = &m {
                    debug(&format!("received {m:?}"));
                }
                self.session.handle(m)
            }
            NetEvent::Disconnected => {
                debug("disconnected from the partner");
                self.session.disconnected()
            }
        };
        self.apply(out);
    }

    /// Sends messages and brings media + UI in line with the session state.
    fn apply(&mut self, out: Out) {
        for m in &out.send {
            if let Msg::Presenter { .. } = m {
                debug(&format!("sending {m:?}"));
            }
            self.net.send(m);
        }
        let s = &self.session;
        self.media.set_peer(
            s.peer
                .as_ref()
                .map(|p| (p.ip, Ports::from_base(p.base_port))),
        );
        if !self.session.is_presenting() && self.media.is_sharing() {
            self.end_local_share();
        }
        let s = &self.session;
        self.media.set_receive_screen(s.peer_is_presenting());
        self.media
            .apply_pads(&s.layout().pads(&s.me, s.peer_id(), s.presenter()));
        self.media.set_overlay(&s.annotations, self.screen_rect());
        if out.has(Event::LayoutChanged)
            && let Err(e) = config::save_layout(&self.cfg_dir, s.layout())
        {
            eprintln!("[2fcast] saving layout: {e}");
        }
        self.refresh_ui();
        if out.has(Event::ResumeSharing) {
            self.resume_sharing();
        }
    }

    /// The partner stopped sharing after taking over from me: share again
    /// what I shared before, if that window still exists.
    fn resume_sharing(&mut self) {
        let Some((src, target)) = self.last_share.clone() else {
            return;
        };
        if let ScreenSource::Window(xid) = src {
            let exists = windows::list()
                .map(|ws| ws.iter().any(|w| w.xid as u64 == xid))
                .unwrap_or(false);
            if !exists {
                debug(&format!("not resuming share: window {xid:#x} is gone"));
                self.last_share = None;
                return;
            }
        }
        debug("partner stopped sharing: sharing my window again");
        self.start_sharing(src, target);
    }

    /// Shared-space rectangle on the canvas, if something is being shown.
    fn screen_rect(&self) -> Option<Rect> {
        let l = self.session.layout();
        (self.session.presenter().is_some() && l.screen.visible).then_some(l.screen.rect)
    }

    /// Whether the peer's camera/voice arrive (`None`: not connected yet or
    /// too early to tell).
    fn receiving_from_peer(&self) -> Option<bool> {
        self.media.peer_media_arrives()
    }

    /// Tells the peer when our view of their media changes.
    fn check_media_arrives(&mut self) {
        let ok = self.receiving_from_peer();
        if let Some(arrives) = ok
            && self.session.peer.is_some()
            && ok != self.reported_receiving
        {
            self.reported_receiving = ok;
            debug(&format!(
                "telling the partner: their media arrives here: {arrives}"
            ));
            self.net.send(&Msg::MediaReceived { ok: arrives });
        }
    }

    /// Plain-words diagnosis of one-way or no media, with the fix.
    fn connection_warning(&self) -> String {
        let Some(peer) = &self.session.peer else {
            return String::new();
        };
        let range = |base: u16| format!("{base}:{}", base + 10);
        let mut parts = vec![];
        if self.receiving_from_peer() == Some(false) {
            parts.push(format!(
                "\u{26a0} Nothing arrives from {} (no camera, no voice). A firewall on THIS computer probably blocks it. \
                 Fix here: sudo ufw allow {}/udp",
                peer.name,
                range(self.cfg.base_port)
            ));
        }
        if self.session.peer_receives_us == Some(false) {
            parts.push(format!(
                "\u{26a0} {0} receives nothing from you. A firewall on {0}'s computer probably blocks it. \
                 Fix there: sudo ufw allow {1}/udp",
                peer.name,
                range(peer.base_port)
            ));
        }
        let broken = self.media.undecodable_streams();
        if !broken.is_empty() {
            parts.push(format!(
                "\u{26a0} {}'s {} arrive(s) but cannot be decoded on this computer (H.264 decoder: {}). \
                 Please send the log: {}",
                peer.name,
                broken.join(" and "),
                crate::media::h264_decoder(),
                crate::log::path().display()
            ));
        }
        parts.join("\n")
    }

    fn refresh_ui(&self) {
        let s = &self.session;
        let mut status = match &s.peer {
            None => "Looking for your partner on the local network…".to_string(),
            Some(p) => format!("Connected to {} ({})", p.name, p.ip),
        };
        if let Some(p) = s.presenter() {
            if s.is_presenting() {
                status += " · you are sharing";
            } else {
                status += &format!(" · {} is sharing", s.display_name(p));
            }
        }
        self.ui.status.set_text(&status);
        let warning = self.connection_warning();
        if warning != self.ui.warning.text().as_str() && !warning.is_empty() {
            debug(&format!("WARNING {warning}"));
        }
        self.ui.warning.set_text(&warning);
        self.ui.warning.set_visible(!warning.is_empty());
        self.ui.stop_share.set_sensitive(s.is_presenting());
        self.update_recording_status();
        let free = self
            .disk_free_bytes()
            .map(|b| format!(" · {} free", config::human_size(b)))
            .unwrap_or_default();
        match self.session.recording_since {
            Some(t) => {
                let secs = t.elapsed().as_secs();
                self.ui.record.set_label(&format!(
                    "■ Stop {:02}:{:02}:{:02}{free}",
                    secs / 3600,
                    secs / 60 % 60,
                    secs % 60
                ));
                self.ui.record.add_css_class("recording");
            }
            None => {
                self.ui.record.set_label(&format!("● Record{free}"));
                self.ui.record.remove_css_class("recording");
            }
        }
        self.ui.area.queue_draw();
    }

    /// "Artem recording 01:05" (hh:mm) for each machine that records; my own
    /// line adds the file size (free space is on the Record button).
    fn update_recording_status(&self) {
        let mut lines: Vec<String> = self
            .session
            .recording_timers()
            .iter()
            .map(|(name, d)| {
                let m = d.as_secs() / 60;
                format!("● {name} recording {:02}:{:02}", m / 60, m % 60)
            })
            .collect();
        if self.session.is_recording()
            && let Some(mine) = lines.first_mut()
            && let Some(size) = self.recording_file_bytes()
        {
            *mine += &format!(" · {}", config::human_size(size));
        }
        let text = lines.join("\n");
        if text != self.ui.recording_status.text().as_str() {
            self.ui.recording_status.set_text(&text);
        }
        self.ui.recording_status.set_visible(!text.is_empty());
    }

    fn recording_file_bytes(&self) -> Option<u64> {
        let path = self.recording_file.as_ref()?;
        std::fs::metadata(path).ok().map(|m| m.len())
    }

    /// Free space on the disk recordings are written to (the output folder,
    /// or its nearest existing parent before the first recording).
    fn disk_free_bytes(&self) -> Option<u64> {
        let dir = match &self.recording_file {
            Some(f) => f.parent()?,
            None => self.cfg.output_dir.ancestors().find(|d| d.exists())?,
        };
        gio::File::for_path(dir)
            .query_filesystem_info(gio::FILE_ATTRIBUTE_FILESYSTEM_FREE, gio::Cancellable::NONE)
            .ok()
            .map(|i| i.attribute_uint64(gio::FILE_ATTRIBUTE_FILESYSTEM_FREE))
    }

    fn fill_share_list(&self, shared: &Shared) {
        let list = &self.ui.share_list;
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        let add = |label: &str, src: ScreenSource, target: Option<Target>| {
            let b = gtk::Button::builder().label(label).has_frame(false).build();
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_max_width_chars(50);
            }
            let a = shared.clone();
            b.connect_clicked(move |_| {
                let (src, target) = (src.clone(), target.clone());
                with(&a, move |app| app.start_sharing(src, target));
            });
            list.append(&b);
        };
        add(
            "🖥  Entire screen",
            ScreenSource::Screen,
            Some(Target::Everything),
        );
        match windows::list() {
            Ok(wins) => {
                for w in wins {
                    let target = w.pid.map(|pid| Target::App {
                        pid,
                        binary: windows::process_name(pid).unwrap_or_default(),
                    });
                    add(&w.title, ScreenSource::Window(w.xid as u64), target);
                }
            }
            Err(e) => list.append(&gtk::Label::new(Some(&format!("Cannot list windows: {e}")))),
        }
    }

    /// Stops capturing locally: screen, its sound, and the window's size.
    fn end_local_share(&mut self) {
        if self.media.is_sharing() {
            debug("stop sharing");
        }
        self.media.stop_screen();
        self.share_audio = None;
        if let Some(saved) = self.shared_window.take() {
            debug(&format!("restoring shared window {saved:?}"));
            if let Err(e) = windows::restore(&saved) {
                eprintln!("[2fcast] restoring the shared window: {e:#}");
            }
        }
    }

    fn start_sharing(&mut self, src: ScreenSource, target: Option<Target>) {
        debug(&format!("start sharing {src:?} (sound of {target:?})"));
        let app = self;
        app.ui.share_popover.popdown();
        app.end_local_share();
        // Shrink the window to the shared-space tile, so it is recorded 1:1.
        if let ScreenSource::Window(xid) = src {
            let r = app.session.layout().screen.rect;
            match windows::shrink(xid as u32, r.w.round() as u32, r.h.round() as u32) {
                Ok(saved) => {
                    debug(&format!("shrank shared window, was {saved:?}"));
                    app.shared_window = saved;
                }
                Err(e) => eprintln!("[2fcast] resizing the shared window: {e:#}"),
            }
        }
        let resume = (src.clone(), target.clone());
        let target = target.filter(|_| app.cfg.share_sound);
        app.share_audio = target.and_then(|t| match ShareAudio::start(t) {
            Ok(s) => Some(s),
            Err(e) => {
                app.ui
                    .error
                    .set_text(&format!("Shared sound unavailable: {e:#}"));
                None
            }
        });
        let monitor = app
            .share_audio
            .as_ref()
            .map(|_| ShareAudio::monitor_source());
        if let Err(e) = app.media.start_screen(&src, monitor.as_deref()) {
            app.share_audio = None;
            app.ui.error.set_text(&format!("Cannot share: {e:#}"));
            return;
        }
        app.ui.error.set_text("");
        app.last_share = Some(resume);
        let out = app.session.start_presenting();
        app.apply(out);
    }

    fn stop_sharing(&mut self) {
        let out = self.session.stop_presenting();
        self.end_local_share();
        self.apply(out);
    }

    /// Asks before stopping a recording, so a stray click can't end it.
    fn confirm_stop_recording(&self, shared: &Shared) {
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&self.ui.window)
            .modal(true)
            .message_type(gtk::MessageType::Question)
            .text("Stop recording?")
            .build();
        dialog.add_button("Continue recording", gtk::ResponseType::Cancel);
        dialog.add_button("Stop recording", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let a = shared.clone();
        dialog.connect_response(move |d, r| {
            d.close();
            if r == gtk::ResponseType::Accept {
                let shared = a.clone();
                with(&a, move |app| {
                    if app.media.is_recording() {
                        app.toggle_recording(&shared);
                    }
                });
            }
        });
        dialog.present();
    }

    fn toggle_recording(&mut self, shared: &Shared) {
        let app = self;
        if let Some(rx) = app.media.stop_recording() {
            app.recording_file = None;
            app.ui.record.set_sensitive(false);
            let out = app.session.set_recording(false);
            app.apply(out);
            let a = shared.clone();
            glib::spawn_future_local(async move {
                let result = rx.recv().await;
                with(&a, move |app| {
                    app.ui.record.set_sensitive(true);
                    match result {
                        Ok(Ok(path)) => {
                            debug(&format!("recording saved: {}", path.display()));
                            app.ui.error.set_text(&format!("Saved {}", path.display()))
                        }
                        Ok(Err(e)) => app.ui.error.set_text(&format!("{e:#}")),
                        Err(_) => {}
                    }
                });
            });
            return;
        }
        let path = config::recording_path(&app.cfg.output_dir, chrono::Local::now().naive_local());
        let (kbps, preset) = (app.cfg.record_bitrate_kbps, app.cfg.record_preset.clone());
        let names = TrackNames {
            me: app.session.my_name.clone(),
            peer: app
                .session
                .peer
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
        };
        match app.media.start_recording(&path, kbps, &preset, &names) {
            Ok(()) => {
                debug(&format!("recording to {}", path.display()));
                app.recording_file = Some(path.clone());
                app.ui.error.set_text("");
                let out = app.session.set_recording(true);
                app.apply(out);
            }
            Err(e) => app.ui.error.set_text(&format!("Cannot record: {e:#}")),
        }
    }

    fn shutdown(&mut self) {
        if let Some(rx) = self.media.stop_recording() {
            let _ = rx.recv_blocking();
        }
        self.end_local_share();
        let out = self.session.stop_presenting();
        for m in &out.send {
            self.net.send(m);
        }
    }

    /// Runs a control-API command through the same code as the buttons.
    fn handle_control(&mut self, cmd: control::Command, shared: &Shared) -> control::Reply {
        use control::Command as C;
        let mut windows_out = vec![];
        let result: Result<(), String> = match cmd {
            C::State => Ok(()),
            C::Windows => windows::list().map_err(|e| format!("{e:#}")).map(|ws| {
                windows_out = ws
                    .into_iter()
                    .map(|w| control::WindowEntry {
                        xid: w.xid,
                        title: w.title,
                    })
                    .collect();
            }),
            C::Share { xid: None } => {
                self.start_sharing(ScreenSource::Screen, Some(Target::Everything));
                Ok(())
            }
            C::Share { xid: Some(xid) } => match windows::list() {
                Err(e) => Err(format!("{e:#}")),
                Ok(ws) => match ws.into_iter().find(|w| w.xid == xid) {
                    None => Err(format!("no shareable window {xid}")),
                    Some(w) => {
                        let target = w.pid.map(|pid| Target::App {
                            pid,
                            binary: windows::process_name(pid).unwrap_or_default(),
                        });
                        self.start_sharing(ScreenSource::Window(xid as u64), target);
                        Ok(())
                    }
                },
            },
            C::StopSharing if self.session.is_presenting() => {
                self.stop_sharing();
                Ok(())
            }
            C::StopSharing => Err("not sharing".into()),
            C::Record { on } => {
                if on != self.media.is_recording() {
                    self.toggle_recording(shared);
                }
                Ok(())
            }
            C::Mute { on } => {
                self.set_muted(on);
                Ok(())
            }
            C::HideSelfView { on } => {
                self.set_self_view_hidden(on);
                Ok(())
            }
            C::Preset { name } => match control::parse_preset(&name) {
                None => Err(format!("unknown preset {name}")),
                Some(p) => {
                    let out = self
                        .session
                        .set_layout(Layout::preset(p, &self.session.active_ids()));
                    self.apply(out);
                    Ok(())
                }
            },
            C::Draw { points } if self.screen_rect().is_some() && !points.is_empty() => {
                let pt = |(x, y): (f32, f32)| Pt { x, y };
                let id = self.ids.next_id();
                self.my_items.push(id);
                let out = self.session.annotate(AnnotOp::StrokeBegin {
                    id,
                    color: self.color,
                    width: STROKE_WIDTH,
                    at: pt(points[0]),
                });
                self.apply(out);
                let out = self.session.annotate(AnnotOp::StrokeTo {
                    id,
                    pts: points[1..].iter().copied().map(pt).collect(),
                });
                self.apply(out);
                Ok(())
            }
            C::Text { x, y, text } if self.screen_rect().is_some() => {
                let id = self.ids.next_id();
                self.my_items.push(id);
                let out = self.session.annotate(AnnotOp::Text {
                    id,
                    color: self.color,
                    size: TEXT_SIZE,
                    at: Pt { x, y },
                    text,
                });
                self.apply(out);
                Ok(())
            }
            C::Arrow {
                from,
                to,
                width,
                shape,
            } if self.screen_rect().is_some() => {
                let id = self.ids.next_id();
                self.my_items.push(id);
                let (from, to) = (
                    Pt {
                        x: from.0,
                        y: from.1,
                    },
                    Pt { x: to.0, y: to.1 },
                );
                let width = width.unwrap_or(self.arrow_width);
                let out = self.session.annotate(AnnotOp::ArrowBegin {
                    id,
                    shape: shape.unwrap_or_default(),
                    color: self.color,
                    width,
                    at: from,
                });
                self.apply(out);
                let out = self.session.annotate(AnnotOp::ArrowSet { id, to, width });
                self.apply(out);
                Ok(())
            }
            C::Ping { x, y } if self.screen_rect().is_some() => {
                self.ping(Pt { x, y });
                Ok(())
            }
            C::Undo => {
                self.undo();
                Ok(())
            }
            C::Draw { .. } | C::Text { .. } | C::Arrow { .. } | C::Ping { .. } => {
                Err("nobody is sharing (or nothing to draw)".into())
            }
            C::Clear => {
                let out = self.session.annotate(AnnotOp::Clear);
                self.apply(out);
                Ok(())
            }
        };
        let state = self.control_state();
        match result {
            Ok(()) => control::Reply {
                windows: windows_out,
                ..control::Reply::ok(state)
            },
            Err(e) => control::Reply::error(e, state),
        }
    }

    fn control_state(&self) -> control::State {
        let s = &self.session;
        let (mine, theirs) = self.media.voice_levels();
        control::State {
            me: s.my_name.clone(),
            peer: s.peer.as_ref().map(|p| p.name.clone()),
            presenter: s.presenter().map(|p| s.display_name(p)),
            presenting: s.is_presenting(),
            shared_window: self.shared_window.as_ref().map(|w| control::SharedWindow {
                xid: w.xid,
                original_width: w.width,
                original_height: w.height,
                was_maximized: w.maximized,
            }),
            recording: self.media.is_recording(),
            peer_recording: s.peer_is_recording(),
            recording_secs: s.recording_since.map(|t| t.elapsed().as_secs()),
            peer_recording_secs: s.peer_recording_since.map(|t| t.elapsed().as_secs()),
            recording_file_bytes: self.recording_file_bytes(),
            disk_free_bytes: self.disk_free_bytes(),
            muted: self.media.is_mic_muted(),
            self_view_hidden: self.prefs.hide_self_view,
            mic_channel: self
                .media
                .has_mic()
                .then(|| format!("{:?}", self.media.mic_pick())),
            my_level_db: mine.map(|l| l.rms_db).filter(|v| v.is_finite()),
            peer_level_db: theirs.map(|l| l.rms_db).filter(|v| v.is_finite()),
            noise_gate_db: self.media.noise_gate_db(),
            annotations: s.annotations.items().len(),
            layout: Some(s.layout().clone()),
            message: self.ui.error.text().to_string(),
            receiving_from_peer: self.receiving_from_peer(),
            peer_receives_us: self.session.peer_receives_us,
            warning: self.connection_warning(),
        }
    }

    fn undo(&mut self) {
        while let Some(id) = self.my_items.pop() {
            if self
                .session
                .annotations
                .items()
                .iter()
                .any(|i| i.id() == id)
            {
                let out = self.session.annotate(AnnotOp::Remove { id });
                self.apply(out);
                return;
            }
        }
    }

    fn toggle_tile(&mut self, which: u8) {
        let id = match which {
            0 => TileId::Screen,
            1 => TileId::Cam(self.session.me.clone()),
            _ => match self.session.peer_id() {
                Some(p) => TileId::Cam(p.clone()),
                None => return,
            },
        };
        let mut l = self.session.layout().clone();
        let visible = l.tile(&id).is_some_and(|t| t.visible);
        l.set_visible(&id, !visible);
        let out = self.session.set_layout(l);
        self.apply(out);
    }

    fn fit(&self) -> Fit {
        Fit::contain(self.ui.area.width() as f64, self.ui.area.height() as f64)
    }

    /// Normalized point on the shared space under widget point (x, y).
    fn screen_point(&self, x: f64, y: f64) -> Option<Pt> {
        let rect = self.screen_rect()?;
        let (cx, cy) = self.fit().to_canvas(x, y);
        rect.contains(cx, cy).then(|| rect.normalize(cx, cy))
    }

    fn drag_begin(&mut self, x: f64, y: f64) {
        debug(&format!(
            "drag begin {x:.0},{y:.0} tool {:?} canvas {:?} screen {:?}",
            self.tool,
            self.fit().to_canvas(x, y),
            self.screen_rect()
        ));
        self.drag_start = (x, y);
        // While muted, a press on the shared space only unmutes.
        self.swallow_gesture = false;
        if self.media.is_mic_muted()
            && let Some(r) = self.mute_hint_rect()
        {
            let (cx, cy) = self.fit().to_canvas(x, y);
            if r.contains(cx, cy) {
                self.set_muted(false);
                self.swallow_gesture = true;
                return;
            }
        }
        match self.tool {
            Tool::Arrow | Tool::Line | Tool::Rect => {
                if let Some(at) = self.screen_point(x, y) {
                    let id = self.ids.next_id();
                    self.arrow = Some((id, at));
                    self.my_items.push(id);
                    let out = self.session.annotate(AnnotOp::ArrowBegin {
                        id,
                        shape: self.tool.shape().unwrap_or_default(),
                        color: self.color,
                        width: self.arrow_width,
                        at,
                    });
                    self.apply(out);
                }
            }
            Tool::Pen => {
                if let Some(at) = self.screen_point(x, y) {
                    let id = self.ids.next_id();
                    self.stroke = Some(id);
                    self.my_items.push(id);
                    let out = self.session.annotate(AnnotOp::StrokeBegin {
                        id,
                        color: self.color,
                        width: STROKE_WIDTH,
                        at,
                    });
                    self.apply(out);
                }
            }
            Tool::Arrange => {
                let (cx, cy) = self.fit().to_canvas(x, y);
                let l = self.session.layout();
                self.drag_tile = l
                    .hit(cx, cy, &self.session.active_ids())
                    .and_then(|id| l.tile(&id).map(|t| (id, t.rect)));
            }
            Tool::Ping | Tool::Text => {}
        }
    }

    fn drag_update(&mut self, dx: f64, dy: f64) {
        if self.swallow_gesture {
            return;
        }
        let (x, y) = (self.drag_start.0 + dx, self.drag_start.1 + dy);
        if let Some((id, _)) = self.arrow {
            let Some(rect) = self.screen_rect() else {
                return;
            };
            let (cx, cy) = self.fit().to_canvas(x, y);
            let to = rect.normalize(cx, cy);
            self.arrow = Some((id, to));
            let out = self.session.annotate(AnnotOp::ArrowSet {
                id,
                to,
                width: self.arrow_width,
            });
            self.apply(out);
        } else if let Some(id) = self.stroke {
            let Some(rect) = self.screen_rect() else {
                return;
            };
            let (cx, cy) = self.fit().to_canvas(x, y);
            let out = self.session.annotate(AnnotOp::StrokeTo {
                id,
                pts: vec![rect.normalize(cx, cy)],
            });
            self.apply(out);
        } else if let Some((id, start)) = self.drag_tile.clone() {
            let scale = self.fit().scale;
            let mut l = self.session.layout().clone();
            l.move_to(&id, start.x + dx / scale, start.y + dy / scale);
            let out = self.session.set_layout(l);
            self.apply(out);
        }
    }

    fn click(&mut self, x: f64, y: f64) {
        if self.swallow_gesture {
            return;
        }
        let Some(at) = self.screen_point(x, y) else {
            return;
        };
        match self.tool {
            Tool::Ping => return self.ping(at),
            Tool::Text => {}
            _ => return,
        }
        self.text_at = Some(at);
        self.typing = None;
        self.ui.text_view.buffer().set_text("");
        self.ui
            .text_popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        self.ui.text_popover.popup();
        self.ui.text_view.grab_focus();
    }

    /// The text being written changed: show it to both of us right away.
    fn typed(&mut self, text: String) {
        let Some(at) = self.text_at else {
            return;
        };
        let out = match self.typing {
            Some(id) => self.session.annotate(AnnotOp::TextSet { id, text }),
            // Created with the first visible character (blank text is dropped).
            None if text.trim().is_empty() => return,
            None => {
                let id = self.ids.next_id();
                self.typing = Some(id);
                self.my_items.push(id);
                self.session.annotate(AnnotOp::Text {
                    id,
                    color: self.color,
                    size: TEXT_SIZE,
                    at,
                    text,
                })
            }
        };
        self.apply(out);
    }

    /// The text popover closed: the text stays, unless it was erased.
    fn finish_text(&mut self) {
        self.text_at = None;
        let Some(id) = self.typing.take() else {
            return;
        };
        let blank = self.session.annotations.items().iter().any(
            |i| matches!(i, Item::Text { id: t, text, .. } if *t == id && text.trim().is_empty()),
        );
        if blank {
            let out = self.session.annotate(AnnotOp::Remove { id });
            self.apply(out);
        }
    }

    fn ping(&mut self, at: Pt) {
        let out = self.session.annotate(AnnotOp::Ping {
            color: self.color,
            at,
        });
        self.apply(out);
    }

    fn clear_drawings(&mut self) {
        let out = self.session.annotate(AnnotOp::Clear);
        self.apply(out);
    }

    fn scroll(&mut self, dy: f64) {
        if self.tool.shape().is_some() {
            // The wheel sets the shape width (also while drawing one).
            let factor = if dy < 0.0 { 1.15 } else { 1.0 / 1.15 };
            self.arrow_width = (self.arrow_width * factor).clamp(ARROW_MIN_WIDTH, ARROW_MAX_WIDTH);
            if let Some((id, to)) = self.arrow {
                let out = self.session.annotate(AnnotOp::ArrowSet {
                    id,
                    to,
                    width: self.arrow_width,
                });
                self.apply(out);
            }
            let px = self.arrow_width * self.session.layout().screen.rect.h as f32;
            self.ui.error.set_text(&format!("Width: {px:.0} px"));
            return;
        }
        if self.tool != Tool::Arrange {
            return;
        }
        let (cx, cy) = self.fit().to_canvas(self.pointer.0, self.pointer.1);
        let mut l = self.session.layout().clone();
        if let Some(id) = l.hit(cx, cy, &self.session.active_ids()) {
            l.scale(&id, if dy < 0.0 { 1.1 } else { 1.0 / 1.1 });
            let out = self.session.set_layout(l);
            self.apply(out);
        }
    }

    /// Level meters for both voices with a dB scale (UI only, not recorded).
    fn draw_meters(&self, cr: &gtk::cairo::Context, w: f64) {
        let (mine, theirs) = self.media.voice_levels();
        let me = if !self.media.has_mic() {
            format!("{} (no mic)", self.session.my_name)
        } else if self.media.is_mic_muted() {
            format!("{} (muted)", self.session.my_name)
        } else {
            self.session.my_name.clone()
        };
        let peer = match &self.session.peer {
            Some(p) => p.name.clone(),
            None => "Partner (not here)".to_string(),
        };
        let (name_w, value_w) = (130.0, 52.0);
        let (x0, x1) = (name_w, w - value_w);
        let x = |db: f64| x0 + levels::fraction(db) * (x1 - x0);
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );

        // Scale.
        cr.set_font_size(9.0);
        for db in levels::TICKS_DB {
            let text = format!("{db:.0}");
            let tw = cr.text_extents(&text).map(|e| e.width()).unwrap_or(0.0);
            cr.set_source_rgba(0.5, 0.5, 0.5, 1.0);
            cr.move_to((x(db) - tw / 2.0).clamp(x0, x1 - tw), 9.0);
            let _ = cr.show_text(&text);
        }
        cr.move_to(x1 + 6.0, 9.0);
        let _ = cr.show_text("dB");

        for (row, (name, level)) in [(me, mine), (peer, theirs)].into_iter().enumerate() {
            let y = 12.0 + row as f64 * 17.0;
            let (bar_y, bar_h) = (y + 2.0, 11.0);
            // My own name in bold (the tooltip says where to change it).
            cr.select_font_face(
                "Sans",
                gtk::cairo::FontSlant::Normal,
                if row == 0 {
                    gtk::cairo::FontWeight::Bold
                } else {
                    gtk::cairo::FontWeight::Normal
                },
            );
            cr.set_font_size(11.0);
            if row == 0 {
                cr.set_source_rgba(0.45, 0.45, 0.45, 1.0);
            } else {
                cr.set_source_rgba(0.55, 0.55, 0.55, 1.0);
            }
            cr.move_to(0.0, y + 11.0);
            let _ = cr.show_text(&ellipsize(cr, &name, name_w - 8.0));
            cr.select_font_face(
                "Sans",
                gtk::cairo::FontSlant::Normal,
                gtk::cairo::FontWeight::Normal,
            );
            // Track with zone tints, so the scale reads even in silence.
            for (from, to, rgb) in [
                (levels::FLOOR_DB, -12.0, ZONE_GREEN),
                (-12.0, -3.0, ZONE_YELLOW),
                (-3.0, 0.0, ZONE_RED),
            ] {
                cr.set_source_rgba(rgb.0, rgb.1, rgb.2, 0.18);
                cr.rectangle(x(from), bar_y, x(to) - x(from), bar_h);
                let _ = cr.fill();
            }
            let level = level.unwrap_or(levels::Level::SILENT);
            // RMS (loudness) as the filled bar, colored by zone segments.
            for (from, to, rgb) in [
                (levels::FLOOR_DB, -12.0, ZONE_GREEN),
                (-12.0, -3.0, ZONE_YELLOW),
                (-3.0, 0.0, ZONE_RED),
            ] {
                let end = level.rms_db.min(to);
                if end > from {
                    cr.set_source_rgb(rgb.0, rgb.1, rgb.2);
                    cr.rectangle(x(from), bar_y, x(end) - x(from), bar_h);
                    let _ = cr.fill();
                }
            }
            // Peak as a thin marker.
            if level.peak_db > levels::FLOOR_DB {
                let rgb = zone_color(levels::zone(level.peak_db));
                cr.set_source_rgb(rgb.0, rgb.1, rgb.2);
                cr.rectangle(x(level.peak_db) - 1.0, bar_y - 1.0, 2.0, bar_h + 2.0);
                let _ = cr.fill();
            }
            // Noise gate threshold on my meter: quieter than this is muted.
            if row == 0
                && let Some(db) = self.media.noise_gate_db()
            {
                cr.set_source_rgba(0.35, 0.35, 0.95, 0.9);
                cr.set_line_width(1.0);
                cr.set_dash(&[2.0, 2.0], 0.0);
                cr.move_to(x(db) + 0.5, bar_y - 2.0);
                cr.line_to(x(db) + 0.5, bar_y + bar_h + 2.0);
                let _ = cr.stroke();
                cr.set_dash(&[], 0.0);
            }
            for db in levels::TICKS_DB {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
                cr.rectangle(x(db), bar_y, 1.0, bar_h);
                let _ = cr.fill();
            }
            cr.set_font_size(10.0);
            cr.set_source_rgba(0.55, 0.55, 0.55, 1.0);
            cr.move_to(x1 + 6.0, y + 11.0);
            let _ = cr.show_text(&levels::label(level.peak_db));
        }
    }

    fn apply_preset(&mut self, p: Preset) {
        let l = Layout::preset(p, &self.session.active_ids());
        let out = self.session.set_layout(l);
        self.apply(out);
    }

    /// Mutes or unmutes the mic, keeping the Mic button in step.
    fn set_muted(&mut self, on: bool) {
        self.media.set_mic_muted(on);
        if let Some(b) = find::<gtk::ToggleButton>(&self.ui.window, "mic") {
            b.set_active(on);
        }
        self.ui.area.queue_draw();
    }

    /// Hides or shows my own camera on my screen only (button, control API).
    fn set_self_view_hidden(&mut self, on: bool) {
        if self.prefs.hide_self_view != on {
            self.prefs.hide_self_view = on;
            if let Err(e) = config::save_ui_prefs(&self.cfg_dir, &self.prefs) {
                debug(&format!("cannot save ui prefs: {e:#}"));
            }
        }
        self.sync_self_view_button(on);
        self.ui.area.queue_draw();
    }

    fn sync_self_view_button(&self, hidden: bool) {
        if let Some(b) = find::<gtk::ToggleButton>(&self.ui.window, "self-view") {
            if b.is_active() != hidden {
                b.set_active(hidden);
            }
            b.set_label(if hidden {
                "Self-view off"
            } else {
                "Self-view on"
            });
        }
    }

    /// Covers my camera tile on my screen (where it is not under the
    /// partner's camera) with a "camera off" sign.
    fn draw_hidden_self_view(&self, cr: &gtk::cairo::Context, fit: &Fit) {
        let s = &self.session;
        let pads = s.layout().pads(&s.me, s.peer_id(), s.presenter());
        let (mine, theirs) = (pads.cam_local, pads.cam_remote);
        if mine.alpha <= 0.0 {
            return;
        }
        let rect =
            |p: PadParams| fit.to_widget(Rect::new(p.x as f64, p.y as f64, p.w as f64, p.h as f64));
        let r = rect(mine);
        cr.save().ok();
        cr.rectangle(r.x, r.y, r.w, r.h);
        cr.clip();
        cr.rectangle(r.x, r.y, r.w, r.h);
        if theirs.alpha > 0.0 && theirs.zorder > mine.zorder {
            let o = rect(theirs);
            cr.rectangle(o.x, o.y, o.w, o.h);
            cr.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
        }
        cr.set_source_rgb(0.16, 0.16, 0.18);
        let _ = cr.fill();
        cr.set_fill_rule(gtk::cairo::FillRule::Winding);

        // Camera body and lens, crossed out.
        let s = (r.w.min(r.h) * 0.45).min(160.0);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (bw, bh) = (s * 0.55, s * 0.4);
        let (bx, by) = (cx - s * 0.4, cy - bh / 2.0);
        cr.set_source_rgba(0.85, 0.85, 0.85, 1.0);
        let rad = bh * 0.18;
        cr.new_sub_path();
        cr.arc(
            bx + bw - rad,
            by + rad,
            rad,
            -std::f64::consts::FRAC_PI_2,
            0.0,
        );
        cr.arc(
            bx + bw - rad,
            by + bh - rad,
            rad,
            0.0,
            std::f64::consts::FRAC_PI_2,
        );
        cr.arc(
            bx + rad,
            by + bh - rad,
            rad,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
        );
        cr.arc(
            bx + rad,
            by + rad,
            rad,
            std::f64::consts::PI,
            1.5 * std::f64::consts::PI,
        );
        cr.close_path();
        let _ = cr.fill();
        cr.move_to(bx + bw + s * 0.03, cy);
        cr.line_to(bx + bw + s * 0.25, cy - bh * 0.4);
        cr.line_to(bx + bw + s * 0.25, cy + bh * 0.4);
        cr.close_path();
        let _ = cr.fill();
        let (a, b) = (
            (cx - s * 0.42, cy - s * 0.38),
            (cx + s * 0.42, cy + s * 0.38),
        );
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_width(s * 0.11);
        cr.set_source_rgb(0.16, 0.16, 0.18);
        cr.move_to(a.0, a.1);
        cr.line_to(b.0, b.1);
        let _ = cr.stroke();
        cr.set_line_width(s * 0.06);
        cr.set_source_rgb(0.9, 0.12, 0.15);
        cr.move_to(a.0, a.1);
        cr.line_to(b.0, b.1);
        let _ = cr.stroke();

        // Caption, if the tile is big enough to read it.
        let text = "Hidden only for you";
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        cr.set_font_size(12.0);
        let tw = cr.text_extents(text).map(|e| e.width()).unwrap_or(0.0);
        if tw + 16.0 < r.w && r.h > s + 40.0 {
            cr.set_source_rgba(0.75, 0.75, 0.75, 1.0);
            cr.move_to(cx - tw / 2.0, cy + s / 2.0 + 18.0);
            let _ = cr.show_text(text);
        }
        cr.restore().ok();
    }

    /// Where the "muted" sign is shown (canvas px): over the shared space, or
    /// the whole picture if the shared space is hidden.
    fn mute_hint_rect(&self) -> Option<Rect> {
        self.media.has_mic().then(|| {
            let l = self.session.layout();
            if l.screen.visible {
                l.screen.rect
            } else {
                Rect::new(0.0, 0.0, CANVAS_W, CANVAS_H)
            }
        })
    }

    /// Big crossed-out microphone, so being muted is impossible to miss.
    fn draw_muted_sign(&self, cr: &gtk::cairo::Context, r: Rect) {
        let s = r.w.min(r.h) * 0.32;
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0 - s * 0.1);
        cr.save().ok();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        cr.arc(cx, cy, s / 2.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
        // Microphone: capsule, holder, stem and base.
        cr.set_source_rgb(1.0, 1.0, 1.0);
        let (mw, mh) = (s * 0.2, s * 0.34);
        let (mx, my) = (cx - mw / 2.0, cy - s * 0.27);
        let rad = mw / 2.0;
        cr.new_sub_path();
        cr.arc(mx + rad, my + rad, rad, std::f64::consts::PI, 0.0);
        cr.arc(mx + rad, my + mh - rad, rad, 0.0, std::f64::consts::PI);
        cr.close_path();
        let _ = cr.fill();
        cr.set_line_width(s * 0.04);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.arc(cx, my + mh - rad, s * 0.17, 0.0, std::f64::consts::PI);
        let _ = cr.stroke();
        cr.move_to(cx, my + mh - rad + s * 0.17);
        cr.line_to(cx, cy + s * 0.27);
        cr.move_to(cx - s * 0.12, cy + s * 0.27);
        cr.line_to(cx + s * 0.12, cy + s * 0.27);
        let _ = cr.stroke();
        // Red slash with a dark edge.
        let (a, b) = ((cx - s * 0.3, cy - s * 0.3), (cx + s * 0.3, cy + s * 0.3));
        cr.set_line_width(s * 0.1);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
        cr.move_to(a.0, a.1);
        cr.line_to(b.0, b.1);
        let _ = cr.stroke();
        cr.set_line_width(s * 0.065);
        cr.set_source_rgb(0.9, 0.12, 0.15);
        cr.move_to(a.0, a.1);
        cr.line_to(b.0, b.1);
        let _ = cr.stroke();
        // Caption.
        let text = "Microphone muted \u{2014} click here to unmute";
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Bold,
        );
        cr.set_font_size((s * 0.11).clamp(12.0, 28.0));
        let tw = cr.text_extents(text).map(|e| e.width()).unwrap_or(0.0);
        let ty = cy + s / 2.0 + s * 0.2;
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
        cr.rectangle(cx - tw / 2.0 - 10.0, ty - s * 0.13, tw + 20.0, s * 0.18);
        let _ = cr.fill();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.move_to(cx - tw / 2.0, ty);
        let _ = cr.show_text(text);
        cr.restore().ok();
    }

    /// UI-only hints drawn over the preview (not recorded).
    fn draw_hints(&self, cr: &gtk::cairo::Context, w: f64, h: f64) {
        let fit = Fit::contain(w, h);
        let s = &self.session;
        let l = s.layout();
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        let label = |text: &str, r: Rect, rgb: (f64, f64, f64)| {
            cr.set_font_size(14.0);
            let ext = cr.text_extents(text).ok();
            let tw = ext.map(|e| e.width()).unwrap_or(0.0);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
            cr.rectangle(r.x + 4.0, r.y + 4.0, tw + 12.0, 22.0);
            let _ = cr.fill();
            cr.set_source_rgb(rgb.0, rgb.1, rgb.2);
            cr.move_to(r.x + 10.0, r.y + 20.0);
            let _ = cr.show_text(text);
        };
        if l.screen.visible && s.presenter().is_none() {
            let r = fit.to_widget(l.screen.rect);
            label(
                "Shared space - nobody is sharing. Click \u{201c}Share\u{2026}\u{201d} to show a window.",
                r,
                (0.8, 0.8, 0.8),
            );
        }
        if self.prefs.hide_self_view && self.media.has_camera() {
            self.draw_hidden_self_view(cr, &fit);
        }
        if self.tool == Tool::Arrange {
            let mut tiles = vec![(String::from("Shared space"), l.screen)];
            for id in s.active_ids() {
                if let Some(t) = l.cams.get(&id) {
                    tiles.push((s.display_name(&id), *t));
                }
            }
            for (name, t) in tiles {
                let r = fit.to_widget(t.rect);
                cr.set_source_rgba(1.0, 0.8, 0.1, if t.visible { 0.9 } else { 0.35 });
                cr.set_line_width(2.0);
                if !t.visible {
                    cr.set_dash(&[6.0, 4.0], 0.0);
                }
                cr.rectangle(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0);
                let _ = cr.stroke();
                cr.set_dash(&[], 0.0);
                let text = if t.visible {
                    name
                } else {
                    format!("{name} (hidden)")
                };
                label(
                    &text,
                    Rect {
                        y: r.y + r.h - 30.0,
                        ..r
                    },
                    (1.0, 0.85, 0.2),
                );
            }
        }
        if self.media.is_mic_muted()
            && let Some(r) = self.mute_hint_rect()
        {
            self.draw_muted_sign(cr, fit.to_widget(r));
        }
    }
}
