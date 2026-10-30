//! User settings (`~/.config/2fcast/config.toml`) and the persisted layout.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::layout::Layout;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Stable random id of this installation.  Generated on first start.
    pub id: String,
    /// Name shown to the other participant.
    pub name: String,
    /// "auto" (first camera), a device like "/dev/video0", "test" or "none".
    pub camera: String,
    /// "default", a PulseAudio source name (see `pactl list sources short`) or "test".
    pub microphone: String,
    /// Which input channel carries the mic: "auto", "both", "left" or "right".
    /// Interfaces like the Scarlett put input 1 on the left, input 2 on the right.
    pub microphone_channel: String,
    /// Silence the mic while you are not speaking (no room noise recorded).
    pub noise_gate: bool,
    /// Gate threshold in dBFS: louder opens it.  Raise it in a noisy room.
    pub noise_gate_db: f64,
    /// Removes rumble below this frequency (Hz) from the mic; 0 = off.
    pub high_pass_hz: f64,
    /// Evens out loud and quiet speech (and limits peaks).
    pub compressor: bool,
    /// How much quieter (dB) the partner is on my speakers while I speak;
    /// 0 = off.  Recordings are not affected.
    pub duck_partner_db: f64,
    /// Also send/record the sound of the shared window (e.g. a video).
    pub share_sound: bool,
    /// Where recordings are written.
    pub output_dir: PathBuf,
    /// TCP control port; UDP media uses the next few even ports.
    pub base_port: u16,
    /// Find the partner automatically on the LAN (mDNS).  Turn off to connect
    /// only via `peer_address` (or not at all, e.g. for tests).
    pub discovery: bool,
    /// Optional "ip:port" of the other machine, for networks without multicast.
    pub peer_address: Option<String>,
    pub record_bitrate_kbps: u32,
    /// x264 speed preset for the recording: ultrafast/superfast/veryfast/faster.
    pub record_preset: String,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let user = std::env::var("USER").unwrap_or_else(|_| "me".into());
        Config {
            id: String::new(),
            name: capitalize(&user),
            camera: "auto".into(),
            microphone: "default".into(),
            microphone_channel: "auto".into(),
            noise_gate: true,
            noise_gate_db: crate::gate::DEFAULT_THRESHOLD_DB,
            high_pass_hz: crate::voice::DEFAULT_HIGH_PASS_HZ,
            compressor: true,
            duck_partner_db: crate::voice::DEFAULT_DUCK_DB,
            share_sound: true,
            output_dir: home.join("Videos").join("2fcast"),
            base_port: 47100,
            discovery: true,
            peer_address: None,
            record_bitrate_kbps: 6000,
            record_preset: "veryfast".into(),
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

pub fn new_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// `$TWOFCAST_CONFIG_DIR`, else `$XDG_CONFIG_HOME/2fcast`, else `~/.config/2fcast`.
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TWOFCAST_CONFIG_DIR") {
        return d.into();
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("2fcast")
}

impl Config {
    /// Loads `config.toml` from `dir`, creating it (with a fresh id) if needed.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Config> {
        let path = dir.join("config.toml");
        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).context(path.display().to_string()),
        };
        if cfg.id.is_empty() || !path.exists() {
            if cfg.id.is_empty() {
                cfg.id = new_id();
            }
            cfg.save(dir)?;
        }
        Ok(cfg)
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("config.toml"), toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

pub fn load_layout(dir: &Path) -> Option<Layout> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("layout.json")).ok()?).ok()
}

pub fn save_layout(dir: &Path, layout: &Layout) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join("layout.json"),
        serde_json::to_string_pretty(layout)?,
    )?;
    Ok(())
}

/// A new, not yet existing file name for a recording started at `now`.
pub fn recording_path(dir: &Path, now: chrono::NaiveDateTime) -> PathBuf {
    let stem = format!("2fcast_{}", now.format("%Y-%m-%d_%H-%M-%S"));
    let mut path = dir.join(format!("{stem}.mkv"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}_{n}.mkv"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Preset;

    #[test]
    fn creates_config_with_id_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(a.id.len(), 16);
        assert!(dir.path().join("config.toml").exists());
        let b = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn partial_file_gets_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "name = \"Ivan\"\nbase_port = 47200\n",
        )
        .unwrap();
        let c = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(c.name, "Ivan");
        assert_eq!(c.base_port, 47200);
        assert_eq!(c.camera, "auto");
        assert_eq!(
            (c.microphone_channel.as_str(), c.noise_gate, c.noise_gate_db),
            ("auto", true, -45.0)
        );
        assert_eq!(
            (c.high_pass_hz, c.compressor, c.duck_partner_db),
            (80.0, true, 6.0)
        );
        assert!(!c.id.is_empty(), "id generated and saved");
        assert!(
            std::fs::read_to_string(dir.path().join("config.toml"))
                .unwrap()
                .contains(&c.id)
        );
    }

    #[test]
    fn invalid_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "base_port = \"x\"").unwrap();
        assert!(Config::load_or_create(dir.path()).is_err());
    }

    #[test]
    fn layout_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_layout(dir.path()).is_none());
        let l = Layout::preset(Preset::CamsOnly, &["x".into()]);
        save_layout(dir.path(), &l).unwrap();
        assert_eq!(load_layout(dir.path()), Some(l));
    }

    #[test]
    fn recording_paths_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let t = chrono::NaiveDate::from_ymd_opt(2026, 10, 29)
            .unwrap()
            .and_hms_opt(14, 5, 9)
            .unwrap();
        let p = recording_path(dir.path(), t);
        assert_eq!(p.file_name().unwrap(), "2fcast_2026-10-29_14-05-09.mkv");
        std::fs::write(&p, "").unwrap();
        assert_eq!(
            recording_path(dir.path(), t).file_name().unwrap(),
            "2fcast_2026-10-29_14-05-09_2.mkv"
        );
    }

    #[test]
    fn capitalize_works() {
        assert_eq!(capitalize("artem"), "Artem");
        assert_eq!(capitalize(""), "");
    }
}
