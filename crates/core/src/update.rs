//! Update check: is a newer 2fcast published on GitHub, and how to install it.
//!
//! At start the app asks GitHub for the latest release (`LATEST_URL`), and if
//! it is newer than the running version and carries a `.deb`, it offers to
//! install it.  The download is checked against the SHA-256 digest GitHub
//! reports for the file, then installed with `pkexec apt-get install` (Ubuntu
//! asks for the password).  Only installed packages check (not `cargo run`).

use std::cmp::Ordering;
use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

pub const REPO: &str = "ostretsov/2fcast";
pub const LATEST_URL: &str = "https://api.github.com/repos/ostretsov/2fcast/releases/latest";
/// Environment variable that replaces [`LATEST_URL`] (for tests).
pub const URL_ENV: &str = "TWOFCAST_UPDATE_URL";
/// The architecture of the `.deb` this build runs from.
pub const ARCH: &str = "amd64";

/// `major.minor.patch`; a leading "v" and any "-suffix" are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split(['-', '+']).next()?;
        let mut parts = s.split('.').map(|p| p.parse::<u64>().ok());
        let v = Version(
            parts.next()??,
            parts.next()??,
            parts.next().unwrap_or(Some(0))?,
        );
        parts.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A published release with a package for this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: Version,
    /// Release page (notes) on GitHub.
    pub page_url: String,
    /// File name of the `.deb`, e.g. "2fcast_0.8.0_amd64.deb".
    pub deb_name: String,
    pub deb_url: String,
    pub deb_size: u64,
    /// Lower-case hex SHA-256 of the `.deb`, if GitHub reports one.
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Parses GitHub's "latest release" JSON.  `Ok(None)`: nothing to install
/// for this architecture (or a draft / pre-release).
pub fn parse_release(json: &str) -> anyhow::Result<Option<Release>> {
    let r: GhRelease = serde_json::from_str(json).context("unexpected reply from GitHub")?;
    if r.draft || r.prerelease {
        return Ok(None);
    }
    let Some(version) = Version::parse(&r.tag_name) else {
        bail!("release tag {:?} is not a version", r.tag_name);
    };
    let suffix = format!("_{ARCH}.deb");
    let Some(a) = r
        .assets
        .into_iter()
        .find(|a| a.name.starts_with("2fcast_") && a.name.ends_with(&suffix))
    else {
        return Ok(None);
    };
    if a.name.contains('/') || !a.browser_download_url.starts_with("https://") {
        bail!("unexpected release file {:?}", a.browser_download_url);
    }
    let sha256 = a
        .digest
        .and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    Ok(Some(Release {
        version,
        page_url: r.html_url,
        deb_name: a.name,
        deb_url: a.browser_download_url,
        deb_size: a.size,
        sha256,
    }))
}

/// The release from `json` if it is newer than `current`.
pub fn newer_release(current: &str, json: &str) -> anyhow::Result<Option<Release>> {
    let current = Version::parse(current).context("own version")?;
    Ok(parse_release(json)?.filter(|r| r.version.cmp(&current) == Ordering::Greater))
}

/// Whether `exe` was installed from a 2fcast `.deb` (so a newer `.deb`
/// replaces it), not a development build.
pub fn is_installed_package(exe: &Path) -> bool {
    exe.starts_with("/opt/2fcast/") || exe == Path::new("/usr/bin/2fcast")
}

/// The command that installs `deb` as root; `pkexec` shows Ubuntu's
/// password dialog.  apt (unlike `dpkg -i`) also installs new dependencies.
pub fn install_command(deb: &Path) -> Vec<String> {
    vec![
        "pkexec".into(),
        "env".into(),
        "DEBIAN_FRONTEND=noninteractive".into(),
        "apt-get".into(),
        "install".into(),
        "-y".into(),
        deb.display().to_string(),
    ]
}

/// What `install_command`'s exit status means.  pkexec exits with 126 when
/// the password dialog is cancelled and 127 when not authorized.
pub fn install_failure(code: Option<i32>) -> Option<&'static str> {
    match code {
        Some(0) => None,
        Some(126) => Some("cancelled"),
        Some(127) => Some("not authorized (an administrator password is needed)"),
        _ => Some("apt-get failed"),
    }
}

/// Shell script that waits for process `pid` to exit (its ports are then
/// free) and starts 2fcast again with the arguments that follow it.
pub fn restart_script(pid: u32) -> String {
    format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; exec /usr/bin/2fcast \"$@\"")
}

/// "23.7 MB".
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(tag: &str, assets: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/{REPO}/releases/tag/{tag}",
                "draft":false,"prerelease":false,"assets":[{assets}]}}"#
        )
    }

    fn deb(name: &str) -> String {
        format!(
            r#"{{"name":"{name}","size":23730648,
                 "digest":"sha256:BCB3AFBB6700EEA21DBB8FDCDBC41A2ABA764A7781EE24E680FF5AD1591073BC",
                 "browser_download_url":"https://github.com/{REPO}/releases/download/v0.8.0/{name}"}}"#
        )
    }

    #[test]
    fn versions() {
        assert_eq!(Version::parse("v0.7.0"), Some(Version(0, 7, 0)));
        assert_eq!(Version::parse("1.2"), Some(Version(1, 2, 0)));
        assert_eq!(Version::parse("0.10.1-rc1"), Some(Version(0, 10, 1)));
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("latest"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(0, 8, 0).to_string(), "0.8.0");
    }

    #[test]
    fn finds_newer_deb() {
        let j = json(
            "v0.8.0",
            &format!(
                "{},{}",
                deb("2fcast_0.8.0_arm64.deb"),
                deb("2fcast_0.8.0_amd64.deb")
            ),
        );
        let r = newer_release("0.7.0", &j).unwrap().unwrap();
        assert_eq!(r.version, Version(0, 8, 0));
        assert_eq!(r.deb_name, "2fcast_0.8.0_amd64.deb");
        assert!(r.deb_url.ends_with("/v0.8.0/2fcast_0.8.0_amd64.deb"));
        assert_eq!(r.deb_size, 23730648);
        assert_eq!(
            r.sha256.as_deref(),
            Some("bcb3afbb6700eea21dbb8fdcdbc41a2aba764a7781ee24e680ff5ad1591073bc")
        );
        assert!(r.page_url.ends_with("/tag/v0.8.0"));
    }

    #[test]
    fn same_or_older_is_not_offered() {
        let j = json("v0.7.0", &deb("2fcast_0.7.0_amd64.deb"));
        assert_eq!(newer_release("0.7.0", &j).unwrap(), None);
        assert_eq!(newer_release("0.8.1", &j).unwrap(), None);
    }

    #[test]
    fn no_package_or_prerelease_is_not_offered() {
        assert_eq!(newer_release("0.7.0", &json("v0.8.0", "")).unwrap(), None);
        let j = json("v0.8.0", &deb("2fcast_0.8.0_amd64.deb"))
            .replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert_eq!(newer_release("0.7.0", &j).unwrap(), None);
    }

    #[test]
    fn missing_or_bad_digest_is_none() {
        let j = json("v0.8.0", &deb("2fcast_0.8.0_amd64.deb")).replace("sha256:BCB3", "md5:BCB3");
        assert_eq!(newer_release("0.7.0", &j).unwrap().unwrap().sha256, None);
    }

    #[test]
    fn bad_replies_are_errors() {
        assert!(newer_release("0.7.0", r#"{"message":"API rate limit exceeded"}"#).is_err());
        assert!(newer_release("0.7.0", &json("nightly", "")).is_err());
        let j = json("v0.8.0", &deb("2fcast_0.8.0_amd64.deb"))
            .replace("https://github", "http://github");
        assert!(newer_release("0.7.0", &j).is_err());
    }

    #[test]
    fn installed_package_paths() {
        assert!(is_installed_package(Path::new(
            "/opt/2fcast/libexec/2fcast"
        )));
        assert!(is_installed_package(Path::new("/usr/bin/2fcast")));
        assert!(!is_installed_package(Path::new(
            "/home/a/2fcast/target/debug/2fcast"
        )));
    }

    #[test]
    fn install_and_restart_commands() {
        let c = install_command(Path::new("/tmp/x/2fcast_0.8.0_amd64.deb"));
        assert_eq!(c[0], "pkexec");
        assert_eq!(c.last().unwrap(), "/tmp/x/2fcast_0.8.0_amd64.deb");
        assert!(c.contains(&"apt-get".to_string()));
        assert_eq!(install_failure(Some(0)), None);
        assert_eq!(install_failure(Some(126)), Some("cancelled"));
        assert!(install_failure(None).is_some());
        let s = restart_script(42);
        assert!(s.starts_with("while kill -0 42 "));
        assert!(s.ends_with("exec /usr/bin/2fcast \"$@\""));
        assert_eq!(megabytes(23_730_648), "23.7 MB");
    }
}
