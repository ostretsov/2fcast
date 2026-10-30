//! Listing X11 application windows that can be shared.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

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

/// A shared window's size before we shrank it, to put it back afterwards.
#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub xid: u32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

struct Wm {
    conn: RustConnection,
    root: Window,
    net_wm_state: Atom,
    max_vert: Atom,
    max_horz: Atom,
}

impl Wm {
    fn connect() -> anyhow::Result<Wm> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let atom = |name: &str| -> anyhow::Result<Atom> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        let (net_wm_state, max_vert, max_horz) = (
            atom("_NET_WM_STATE")?,
            atom("_NET_WM_STATE_MAXIMIZED_VERT")?,
            atom("_NET_WM_STATE_MAXIMIZED_HORZ")?,
        );
        Ok(Wm {
            conn,
            root,
            net_wm_state,
            max_vert,
            max_horz,
        })
    }

    fn is_maximized(&self, xid: u32) -> anyhow::Result<bool> {
        let state: Vec<Atom> = self
            .conn
            .get_property(false, xid, self.net_wm_state, AtomEnum::ATOM, 0, 32)?
            .reply()?
            .value32()
            .map(|it| it.collect())
            .unwrap_or_default();
        Ok(state.contains(&self.max_vert) || state.contains(&self.max_horz))
    }

    /// Asks the window manager to (un)maximize, as a pager would (EWMH).
    fn set_maximized(&self, xid: u32, on: bool) -> anyhow::Result<()> {
        let data = [u32::from(on), self.max_vert, self.max_horz, 2, 0];
        let ev = ClientMessageEvent::new(32, xid, self.net_wm_state, data);
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        )?;
        self.conn.sync()?;
        Ok(())
    }

    fn resize(&self, xid: u32, w: u32, h: u32) -> anyhow::Result<()> {
        self.conn
            .configure_window(xid, &ConfigureWindowAux::new().width(w).height(h))?;
        self.conn.sync()?;
        Ok(())
    }
}

/// Shrinks a window so it fits `max_w`x`max_h` (never enlarges it).  Returns
/// what to restore later, or `None` if nothing was changed.
pub fn shrink(xid: u32, max_w: u32, max_h: u32) -> anyhow::Result<Option<Saved>> {
    let wm = Wm::connect()?;
    let geo = wm.conn.get_geometry(xid)?.reply()?;
    let (w, h) = (geo.width as u32, geo.height as u32);
    let Some((nw, nh)) = twofcast_core::layout::shrink_to_fit(w, h, max_w, max_h) else {
        return Ok(None);
    };
    let maximized = wm.is_maximized(xid)?;
    if maximized {
        // A maximized window ignores resize requests.
        wm.set_maximized(xid, false)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    wm.resize(xid, nw, nh)?;
    // Give the window manager and the app a moment before capture starts.
    std::thread::sleep(Duration::from_millis(150));
    Ok(Some(Saved {
        xid,
        width: w,
        height: h,
        maximized,
    }))
}

/// Puts a window back the way [`shrink`] found it.
pub fn restore(saved: &Saved) -> anyhow::Result<()> {
    let wm = Wm::connect()?;
    wm.resize(saved.xid, saved.width, saved.height)?;
    if saved.maximized {
        wm.set_maximized(saved.xid, true)?;
    }
    Ok(())
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
