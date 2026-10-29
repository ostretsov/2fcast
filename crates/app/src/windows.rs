//! Listing X11 application windows that can be shared.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub xid: u32,
    pub title: String,
    pub pid: Option<u32>,
}

/// Windows managed by the window manager, most recently raised first.
/// Our own windows, docks and the desktop are left out.
pub fn list() -> anyhow::Result<Vec<WindowInfo>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;
    let atom = |name: &str| -> anyhow::Result<Atom> {
        Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    };
    let stacking = atom("_NET_CLIENT_LIST_STACKING")?;
    let net_wm_name = atom("_NET_WM_NAME")?;
    let utf8 = atom("UTF8_STRING")?;
    let net_wm_pid = atom("_NET_WM_PID")?;
    let wm_type = atom("_NET_WM_WINDOW_TYPE")?;
    let skip_types = [
        atom("_NET_WM_WINDOW_TYPE_DESKTOP")?,
        atom("_NET_WM_WINDOW_TYPE_DOCK")?,
    ];

    let ids: Vec<Window> = conn
        .get_property(false, root, stacking, AtomEnum::WINDOW, 0, u32::MAX)?
        .reply()?
        .value32()
        .map(|it| it.collect())
        .unwrap_or_default();
    let own_pid = std::process::id();
    let mut out = vec![];
    for id in ids.into_iter().rev() {
        let types: Vec<Atom> = conn
            .get_property(false, id, wm_type, AtomEnum::ATOM, 0, 32)?
            .reply()?
            .value32()
            .map(|it| it.collect())
            .unwrap_or_default();
        if types.iter().any(|t| skip_types.contains(t)) {
            continue;
        }
        let pid = conn
            .get_property(false, id, net_wm_pid, AtomEnum::CARDINAL, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut it| it.next());
        if pid == Some(own_pid) {
            continue;
        }
        let mut title = String::from_utf8_lossy(
            &conn
                .get_property(false, id, net_wm_name, utf8, 0, 1024)?
                .reply()?
                .value,
        )
        .into_owned();
        if title.is_empty() {
            title = String::from_utf8_lossy(
                &conn
                    .get_property(false, id, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)?
                    .reply()?
                    .value,
            )
            .into_owned();
        }
        if !title.trim().is_empty() {
            out.push(WindowInfo {
                xid: id,
                title,
                pid,
            });
        }
    }
    Ok(out)
}

/// Kernel name of a process (`/proc/<pid>/comm`).
pub fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_name() {
        assert!(process_name(std::process::id()).is_some_and(|n| !n.is_empty()));
        assert_eq!(process_name(u32::MAX), None);
    }

    #[test]
    fn lists_windows_when_a_display_exists() {
        if std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let wins = list().expect("X11 query works");
        assert!(wins.iter().all(|w| !w.title.trim().is_empty()));
    }
}
