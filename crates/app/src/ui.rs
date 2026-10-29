//! The GTK user interface and the glue between session, network and media.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use twofcast_core::annotations::{AnnotOp, IdGen, PALETTE, Pt, Rgb, STROKE_WIDTH, TEXT_SIZE};
use twofcast_core::config::{self, Config};
use twofcast_core::deps;
use twofcast_core::layout::{Fit, Layout, Preset, Rect, TileId};
use twofcast_core::net::{Net, NetConfig, NetEvent};
use twofcast_core::pipelines::{Channels, Ports, ScreenSource};
use twofcast_core::pulse::Target;
use twofcast_core::session::{Event, Out, Session};

use crate::media::{Media, MediaConfig, camera_source, mic_source};
use crate::share_audio::{self, ShareAudio};
use crate::windows;

const APP_ID: &str = "org.twofcast.TwoFCast";

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
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Pen,
    Text,
    Arrange,
}

struct Ui {
    window: gtk::ApplicationWindow,
    status: gtk::Label,
    error: gtk::Label,
    share_list: gtk::Box,
    share_popover: gtk::Popover,
    stop_share: gtk::Button,
    record: gtk::Button,
    picture: gtk::Picture,
    area: gtk::DrawingArea,
    text_popover: gtk::Popover,
    text_entry: gtk::Entry,
}

pub struct App {
    cfg: Config,
    cfg_dir: PathBuf,
    session: Session,
    media: Media,
    net: Net,
    share_audio: Option<ShareAudio>,
    ids: IdGen,
    tool: Tool,
    color: Rgb,
    stroke: Option<u64>,
    drag_start: (f64, f64),
    drag_tile: Option<(TileId, Rect)>,
    pointer: (f64, f64),
    my_items: Vec<u64>,
    text_at: Option<Pt>,
    rec_started: Option<Instant>,
    frame_serial: u64,
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
    if std::env::var_os("TWOFCAST_DEBUG").is_some() {
        eprintln!("[2fcast] {msg}");
    }
}

pub fn run() -> glib::ExitCode {
    let mut cfg_dir = config::config_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config-dir" => cfg_dir = args.next().map(PathBuf::from).unwrap_or(cfg_dir),
            "-h" | "--help" => {
                println!(
                    "2fcast - two people, one shared screen, one recording.\n\nUsage: 2fcast [--config-dir DIR]\n\nSettings: {}/config.toml",
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
        if let Err(e) = build(gapp, cfg_dir.clone()) {
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
        .label(cmd.join(" "))
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let button = gtk::Button::with_label("Install");
    button.add_css_class("suggested-action");
    let result = gtk::Label::new(None);
    vbox.append(&label);
    vbox.append(&cmd_label);
    vbox.append(&button);
    vbox.append(&result);
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
            let status = std::process::Command::new(&cmd[0]).args(&cmd[1..]).status();
            let _ = tx.send_blocking(status.map(|s| s.success()).unwrap_or(false));
        });
        let result = result.clone();
        let b = b.clone();
        glib::spawn_future_local(async move {
            if rx.recv().await.unwrap_or(false) {
                use std::os::unix::process::CommandExt;
                // Restart so GStreamer sees the new plugins.
                let exe = std::env::current_exe().unwrap_or_else(|_| "2fcast".into());
                let err = std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec();
                result.set_text(&format!("Installed. Please start 2fcast again ({err})."));
            } else {
                result.set_text("Installation failed or was cancelled. You can run the command above in a terminal.");
                b.set_sensitive(true);
            }
        });
    });
    window.present();
}

fn build(gapp: &gtk::Application, cfg_dir: PathBuf) -> anyhow::Result<()> {
    let cfg = Config::load_or_create(&cfg_dir)?;
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
            playback: true,
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
            discovery: true,
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

    let ui = build_widgets(gapp, &cfg);
    let app: Shared = Rc::new(RefCell::new(App {
        cfg,
        cfg_dir,
        session,
        media,
        net,
        share_audio: None,
        ids: IdGen::new(),
        tool: Tool::Pen,
        color: PALETTE[0],
        stroke: None,
        drag_start: (0.0, 0.0),
        drag_tile: None,
        pointer: (0.0, 0.0),
        my_items: vec![],
        text_at: None,
        rec_started: None,
        frame_serial: 0,
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
    let a = app.clone();
    glib::timeout_add_local(Duration::from_millis(1000), move || {
        if let Ok(app) = a.try_borrow() {
            if let Some(s) = &app.share_audio {
                s.poll();
            }
            app.refresh_ui();
        }
        glib::ControlFlow::Continue
    });
    app.borrow().ui.window.present();
    Ok(())
}

fn build_widgets(gapp: &gtk::Application, cfg: &Config) -> Ui {
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
    let layout_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    layout_box.set_widget_name("layout-box");
    let layout_button = gtk::MenuButton::builder()
        .label("Layout")
        .popover(&gtk::Popover::builder().child(&layout_box).build())
        .build();
    header.pack_end(&record);
    header.pack_end(&mic);
    header.pack_end(&layout_button);
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

    let text_entry = gtk::Entry::builder()
        .placeholder_text("Text, then Enter")
        .width_chars(30)
        .build();
    let text_popover = gtk::Popover::builder().child(&text_entry).build();
    text_popover.set_parent(&area);

    let error = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    error.add_css_class("error-bar");
    let footer = gtk::Label::builder()
        .label(format!(
            "You are {} · recordings go to {}",
            cfg.name,
            cfg.output_dir.display()
        ))
        .xalign(1.0)
        .css_classes(["dim-label"])
        .margin_end(8)
        .hexpand(true)
        .build();
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bottom.append(&error);
    bottom.append(&footer);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&tools);
    vbox.append(&overlay);
    vbox.append(&bottom);
    window.set_child(Some(&vbox));

    Ui {
        window,
        status,
        error,
        share_list,
        share_popover,
        stop_share,
        record,
        picture,
        area,
        text_popover,
        text_entry,
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
    for (tool, label, tip) in [
        (Tool::Pen, "✎ Pen", "Draw over the shared space"),
        (Tool::Text, "T Text", "Click on the shared space to write"),
        (
            Tool::Arrange,
            "⬚ Arrange",
            "Drag tiles to move them, scroll to resize",
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
        tools.append(&b);
    }
    tools.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    let mut first: Option<gtk::ToggleButton> = None;
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
                with(&a, move |app| app.color = color);
            }
        });
        tools.append(&b);
    }
    tools.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    let undo = gtk::Button::with_label("Undo");
    let a = app.clone();
    undo.connect_clicked(move |_| with(&a, |app| app.undo()));
    tools.append(&undo);
    let clear = gtk::Button::with_label("Clear drawings");
    let a = app.clone();
    clear.connect_clicked(move |_| {
        with(&a, |app| {
            let out = app.session.annotate(AnnotOp::Clear);
            app.apply(out);
        })
    });
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
        with(&a, move |app| app.toggle_recording(&shared));
    });
    if !app.borrow().media.has_mic() {
        mic.set_label("No mic");
        mic.set_sensitive(false);
    }
    let a = app.clone();
    mic.connect_toggled(move |b| {
        let muted = b.is_active();
        with(&a, move |app| app.media.set_mic_muted(muted));
        b.set_label(if b.is_active() { "Mic muted" } else { "Mic on" });
    });
    for p in Preset::ALL {
        let b = gtk::Button::with_label(p.label());
        let a = app.clone();
        b.connect_clicked(move |_| {
            with(&a, move |app| {
                let l = Layout::preset(p, &app.session.active_ids());
                let out = app.session.set_layout(l);
                app.apply(out);
            })
        });
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

    let drag = gtk::GestureDrag::new();
    let a = app.clone();
    drag.connect_drag_begin(move |_, x, y| with(&a, move |app| app.drag_begin(x, y)));
    let a = app.clone();
    drag.connect_drag_update(move |_, dx, dy| with(&a, move |app| app.drag_update(dx, dy)));
    let a = app.clone();
    drag.connect_drag_end(move |_, _, _| {
        with(&a, |app| {
            app.stroke = None;
            app.drag_tile = None;
        })
    });
    area.add_controller(drag);

    let click = gtk::GestureClick::new();
    let a = app.clone();
    click.connect_released(move |_, _, x, y| with(&a, move |app| app.click(x, y)));
    area.add_controller(click);

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

    let entry = app.borrow().ui.text_entry.clone();
    let a = app.clone();
    entry.connect_activate(move |e| {
        let text = e.text().to_string();
        with(&a, move |app| {
            app.ui.text_popover.popdown();
            if let Some(at) = app.text_at.take() {
                let id = app.ids.next_id();
                app.my_items.push(id);
                let color = app.color;
                let out = app.session.annotate(AnnotOp::Text {
                    id,
                    color,
                    size: TEXT_SIZE,
                    at,
                    text,
                });
                app.apply(out);
            }
        })
    });

    let a = app.clone();
    window.connect_close_request(move |_| {
        with(&a, |app| app.shutdown());
        glib::Propagation::Proceed
    });
}

impl App {
    fn on_net(&mut self, ev: NetEvent) {
        let out = match ev {
            NetEvent::Connected(peer) => self.session.connected(peer),
            NetEvent::Message(m) => self.session.handle(m),
            NetEvent::Disconnected => self.session.disconnected(),
        };
        self.apply(out);
    }

    /// Sends messages and brings media + UI in line with the session state.
    fn apply(&mut self, out: Out) {
        for m in &out.send {
            self.net.send(m);
        }
        let s = &self.session;
        self.media.set_peer(
            s.peer
                .as_ref()
                .map(|p| (p.ip, Ports::from_base(p.base_port))),
        );
        if !s.is_presenting() && self.media.is_sharing() {
            self.media.stop_screen();
            self.share_audio = None;
        }
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
    }

    /// Shared-space rectangle on the canvas, if something is being shown.
    fn screen_rect(&self) -> Option<Rect> {
        let l = self.session.layout();
        (self.session.presenter().is_some() && l.screen.visible).then_some(l.screen.rect)
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
        if s.peer_recording {
            status += &format!(
                " · ● {} is recording",
                s.peer
                    .as_ref()
                    .map(|p| p.name.as_str())
                    .unwrap_or("partner")
            );
        }
        self.ui.status.set_text(&status);
        self.ui.stop_share.set_sensitive(s.is_presenting());
        match self.rec_started {
            Some(t) => {
                let secs = t.elapsed().as_secs();
                self.ui.record.set_label(&format!(
                    "■ Stop {:02}:{:02}:{:02}",
                    secs / 3600,
                    secs / 60 % 60,
                    secs % 60
                ));
                self.ui.record.add_css_class("recording");
            }
            None => {
                self.ui.record.set_label("● Record");
                self.ui.record.remove_css_class("recording");
            }
        }
        self.ui.area.queue_draw();
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

    fn start_sharing(&mut self, src: ScreenSource, target: Option<Target>) {
        let app = self;
        app.ui.share_popover.popdown();
        app.share_audio = None;
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
        let out = app.session.start_presenting();
        app.apply(out);
    }

    fn stop_sharing(&mut self) {
        let out = self.session.stop_presenting();
        self.media.stop_screen();
        self.share_audio = None;
        self.apply(out);
    }

    fn toggle_recording(&mut self, shared: &Shared) {
        let app = self;
        if let Some(rx) = app.media.stop_recording() {
            app.rec_started = None;
            app.ui.record.set_sensitive(false);
            let out = app.session.set_recording(false);
            app.apply(out);
            let a = shared.clone();
            glib::spawn_future_local(async move {
                let result = rx.recv().await;
                with(&a, move |app| {
                    app.ui.record.set_sensitive(true);
                    match result {
                        Ok(Ok(path)) => app.ui.error.set_text(&format!("Saved {}", path.display())),
                        Ok(Err(e)) => app.ui.error.set_text(&format!("{e:#}")),
                        Err(_) => {}
                    }
                });
            });
            return;
        }
        let path = config::recording_path(&app.cfg.output_dir, chrono::Local::now().naive_local());
        let (kbps, preset) = (app.cfg.record_bitrate_kbps, app.cfg.record_preset.clone());
        match app.media.start_recording(&path, kbps, &preset) {
            Ok(()) => {
                app.rec_started = Some(Instant::now());
                app.ui
                    .error
                    .set_text(&format!("Recording to {}", path.display()));
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
        self.media.stop_screen();
        self.share_audio = None;
        let out = self.session.stop_presenting();
        for m in &out.send {
            self.net.send(m);
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
        match self.tool {
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
            Tool::Text => {}
        }
    }

    fn drag_update(&mut self, dx: f64, dy: f64) {
        let (x, y) = (self.drag_start.0 + dx, self.drag_start.1 + dy);
        if let Some(id) = self.stroke {
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
        if self.tool != Tool::Text {
            return;
        }
        let Some(at) = self.screen_point(x, y) else {
            return;
        };
        self.text_at = Some(at);
        self.ui.text_entry.set_text("");
        self.ui
            .text_popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        self.ui.text_popover.popup();
        self.ui.text_entry.grab_focus();
    }

    fn scroll(&mut self, dy: f64) {
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
    }
}
