//! Session log: `~/.cache/2fcast/2fcast.log` (the previous run is kept as
//! `2fcast.prev.log`).  Always written, so a problem seen on a real machine
//! can be diagnosed afterwards; also printed to stderr with `TWOFCAST_DEBUG=1`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
        })
        .join("2fcast")
}

pub fn path() -> PathBuf {
    dir().join("2fcast.log")
}

/// Starts a new log file (keeping the previous one) and writes a header.
pub fn init() {
    let path = path();
    let _ = std::fs::create_dir_all(dir());
    let _ = std::fs::rename(&path, dir().join("2fcast.prev.log"));
    *FILE.lock().unwrap() = std::fs::File::create(&path).ok();
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| "unknown OS".into());
    write(&format!("2fcast {} on {os}", env!("CARGO_PKG_VERSION")));
}

/// Appends a timestamped line.
pub fn write(msg: &str) {
    let line = format!(
        "{} {msg}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
    );
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
    if std::env::var_os("TWOFCAST_DEBUG").is_some() {
        eprint!("[2fcast] {line}");
    }
}
