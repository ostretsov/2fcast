//! "A new version is available" banner: checks GitHub at start, and on one
//! click downloads the `.deb`, verifies it and installs it (Ubuntu asks for
//! the password), then offers to restart.  The logic is in
//! `twofcast_core::update`.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, bail};
use gtk::glib;
use gtk::prelude::*;
use sha2::{Digest, Sha256};
use twofcast_core::update::{self, Release};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn user_agent() -> String {
    format!("2fcast/{VERSION} (+https://github.com/{})", update::REPO)
}

/// Asks GitHub for the latest release (blocking).
pub fn fetch_newer() -> anyhow::Result<Option<Release>> {
    let url = std::env::var(update::URL_ENV).unwrap_or_else(|_| update::LATEST_URL.into());
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .new_agent();
    let json = agent
        .get(&url)
        .header("User-Agent", user_agent())
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("cannot reach {url}"))?
        .body_mut()
        .read_to_string()?;
    update::newer_release(VERSION, &json)
}

/// A new directory only this run uses, readable by apt's `_apt` user.
fn fresh_dir() -> anyhow::Result<PathBuf> {
    for _ in 0..10 {
        let dir = std::env::temp_dir().join(format!(
            "2fcast-update-{}-{:08x}",
            std::process::id(),
            rand_u32()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context(dir.display().to_string()),
        }
    }
    bail!(
        "cannot create a download folder in {}",
        std::env::temp_dir().display()
    )
}

fn rand_u32() -> u32 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.subsec_nanos() ^ (t.as_secs() as u32).rotate_left(16)
}

/// Downloads the release's `.deb` into `dir` and checks its SHA-256;
/// `progress` gets the bytes received so far.
pub fn download(r: &Release, dir: &Path, mut progress: impl FnMut(u64)) -> anyhow::Result<PathBuf> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .build()
        .new_agent();
    let mut resp = agent
        .get(&r.deb_url)
        .header("User-Agent", user_agent())
        .call()
        .with_context(|| format!("cannot download {}", r.deb_url))?;
    let path = dir.join(&r.deb_name);
    let mut file = std::fs::File::create(&path).context(path.display().to_string())?;
    let mut reader = resp.body_mut().as_reader();
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).context("download interrupted")?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(done);
    }
    file.sync_all()?;
    if r.deb_size > 0 && done != r.deb_size {
        bail!("download incomplete ({done} of {} bytes)", r.deb_size);
    }
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    match &r.sha256 {
        Some(want) if *want != got => bail!("the download is damaged (SHA-256 mismatch)"),
        Some(_) => {}
        None => crate::log::write("update: GitHub reports no SHA-256 for the package"),
    }
    Ok(path)
}

/// Installs `deb` with `pkexec apt-get` (blocking; shows the password dialog).
pub fn install(deb: &Path) -> anyhow::Result<()> {
    let cmd = update::install_command(deb);
    let out = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", cmd[0]))?;
    crate::log::write(&format!(
        "update: {} -> {}\n{}{}",
        cmd.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ));
    if let Some(why) = update::install_failure(out.status.code()) {
        let detail = String::from_utf8_lossy(&out.stderr)
            .lines()
            .rfind(|l| l.starts_with("E:"))
            .map(|l| format!(": {}", l.trim_start_matches("E:").trim()))
            .unwrap_or_default();
        bail!("{why}{detail}");
    }
    Ok(())
}

/// Starts the (new) 2fcast with the same arguments once this process ended.
fn spawn_restarter() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    std::process::Command::new("sh")
        .arg("-c")
        .arg(update::restart_script(std::process::id()))
        .arg("2fcast")
        .args(std::env::args().skip(1))
        .stdin(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

enum Step {
    Found(Release),
    Progress(u64),
    Installing,
    Installed,
    Failed(String),
}

#[derive(Default)]
struct Inner {
    release: Option<Release>,
    busy: bool,
    installed: bool,
}

/// The banner above the shared space; hidden until a newer release is found.
#[derive(Clone)]
pub struct Updater {
    pub root: gtk::Box,
    label: gtk::Label,
    install: gtk::Button,
    restart: gtk::Button,
    close: gtk::Button,
    inner: Rc<RefCell<Inner>>,
    /// Whether this machine is recording (no restart then).
    recording: Rc<RefCell<Box<dyn Fn() -> bool>>>,
}

impl Updater {
    pub fn new() -> Updater {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .css_classes(["update-bar"])
            .build();
        let label = gtk::Label::builder()
            .use_markup(true)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .build();
        label.connect_activate_link(|_, uri| {
            if let Err(e) = std::process::Command::new("xdg-open").arg(uri).spawn() {
                crate::log::write(&format!("cannot open {uri}: {e}"));
            }
            glib::Propagation::Stop
        });
        let install = gtk::Button::with_label("Install");
        install.add_css_class("suggested-action");
        let restart = gtk::Button::builder()
            .label("Restart 2fcast")
            .visible(false)
            .build();
        restart.add_css_class("suggested-action");
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.set_tooltip_text(Some("Not now (asks again at the next start)"));
        close.add_css_class("flat");
        root.append(&label);
        root.append(&install);
        root.append(&restart);
        root.append(&close);
        let u = Updater {
            root,
            label,
            install,
            restart,
            close,
            inner: Rc::default(),
            recording: Rc::new(RefCell::new(Box::new(|| false))),
        };
        let me = u.clone();
        u.install.connect_clicked(move |_| me.start_install());
        let me = u.clone();
        u.restart.connect_clicked(move |b| me.restart(b));
        let me = u.clone();
        u.close.connect_clicked(move |_| me.root.set_visible(false));
        u
    }

    pub fn set_recording_check(&self, f: impl Fn() -> bool + 'static) {
        *self.recording.borrow_mut() = Box::new(f);
    }

    /// Checks GitHub in the background (at start only for installed packages
    /// unless `force`).
    pub fn check(&self, force: bool) {
        let exe = std::env::current_exe().unwrap_or_default();
        if !force && !update::is_installed_package(&exe) {
            crate::log::write(&format!(
                "update: not checking, {} is not an installed package",
                exe.display()
            ));
            return;
        }
        if self.inner.borrow().busy {
            return;
        }
        self.run(|| match fetch_newer() {
            Ok(Some(r)) => vec![Step::Found(r)],
            Ok(None) => {
                crate::log::write(&format!("update: {VERSION} is the latest version"));
                vec![]
            }
            Err(e) => {
                // Offline or GitHub unreachable: not worth bothering the user.
                crate::log::write(&format!("update check failed: {e:#}"));
                vec![]
            }
        });
    }

    /// Runs `work` on a thread and applies the steps it returns, plus any
    /// it sends while running, on the GTK thread.
    fn run(&self, work: impl FnOnce() -> Vec<Step> + Send + 'static) {
        self.run_with(move |_| work());
    }

    fn run_with(
        &self,
        work: impl FnOnce(&async_channel::Sender<Step>) -> Vec<Step> + Send + 'static,
    ) {
        let (tx, rx) = async_channel::unbounded::<Step>();
        std::thread::spawn(move || {
            for s in work(&tx) {
                let _ = tx.send_blocking(s);
            }
        });
        let me = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(step) = rx.recv().await {
                me.apply(step);
            }
        });
    }

    fn apply(&self, step: Step) {
        let mut inner = self.inner.borrow_mut();
        match step {
            Step::Found(r) => {
                crate::log::write(&format!(
                    "update: {} is available ({})",
                    r.version, r.deb_url
                ));
                self.label.set_markup(&format!(
                    "<b>2fcast {} is available</b> (you have {VERSION}). \
                     <a href=\"{}\">What's new</a> · Install it now? ({}; Ubuntu asks for your password)",
                    r.version,
                    glib::markup_escape_text(&r.page_url),
                    update::megabytes(r.deb_size),
                ));
                self.install.set_visible(true);
                self.install.set_sensitive(true);
                self.restart.set_visible(false);
                self.root.set_visible(true);
                inner.release = Some(r);
            }
            Step::Progress(done) => {
                let total = inner.release.as_ref().map_or(0, |r| r.deb_size);
                let pct = (done * 100).checked_div(total).unwrap_or(0);
                self.label.set_text(&format!(
                    "Downloading 2fcast {}… {pct}%",
                    inner
                        .release
                        .as_ref()
                        .map(|r| r.version.to_string())
                        .unwrap_or_default()
                ));
            }
            Step::Installing => {
                self.label
                    .set_text("Installing… enter your password in the dialog Ubuntu shows.");
            }
            Step::Installed => {
                inner.busy = false;
                inner.installed = true;
                let v = inner
                    .release
                    .as_ref()
                    .map(|r| r.version.to_string())
                    .unwrap_or_default();
                crate::log::write(&format!("update: {v} installed"));
                self.label.set_markup(&format!(
                    "<b>2fcast {v} is installed.</b> Restart 2fcast to use it."
                ));
                self.install.set_visible(false);
                self.restart.set_visible(true);
                self.close.set_sensitive(true);
            }
            Step::Failed(e) => {
                inner.busy = false;
                crate::log::write(&format!("update failed: {e}"));
                self.label
                    .set_text(&format!("The update was not installed: {e}"));
                self.install.set_label("Try again");
                self.install.set_sensitive(true);
                self.close.set_sensitive(true);
            }
        }
    }

    fn start_install(&self) {
        let Some(r) = self.inner.borrow().release.clone() else {
            return;
        };
        {
            let mut inner = self.inner.borrow_mut();
            if inner.busy {
                return;
            }
            inner.busy = true;
        }
        self.install.set_sensitive(false);
        self.close.set_sensitive(false);
        self.apply(Step::Progress(0));
        self.run_with(move |tx| {
            let result = (|| {
                let dir = fresh_dir()?;
                let mut last_pct = u64::MAX;
                let deb = download(&r, &dir, |done| {
                    let pct = done * 100 / r.deb_size.max(1);
                    if pct != last_pct {
                        last_pct = pct;
                        let _ = tx.send_blocking(Step::Progress(done));
                    }
                });
                let deb = match deb {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&dir);
                        return Err(e);
                    }
                };
                let _ = tx.send_blocking(Step::Installing);
                let res = install(&deb);
                let _ = std::fs::remove_dir_all(&dir);
                res
            })();
            vec![match result {
                Ok(()) => Step::Installed,
                Err(e) => Step::Failed(format!("{e:#}")),
            }]
        });
    }

    fn restart(&self, button: &gtk::Button) {
        if (self.recording.borrow())() {
            self.label
                .set_text("Stop the recording first, then click Restart 2fcast.");
            return;
        }
        if let Err(e) = spawn_restarter() {
            self.label.set_text(&format!(
                "Cannot restart: {e}. Start 2fcast again yourself."
            ));
            return;
        }
        // Closing the window shuts down like the close button (sharing ends,
        // the partner is told) and ends the process; the new one starts then.
        if let Some(w) = button.root().and_downcast::<gtk::Window>() {
            w.close();
        }
    }

    /// Banner text for the control API ("" while hidden).
    pub fn status(&self) -> String {
        if self.root.is_visible() {
            self.label.text().to_string()
        } else {
            String::new()
        }
    }
}

impl Default for Updater {
    fn default() -> Self {
        Updater::new()
    }
}
