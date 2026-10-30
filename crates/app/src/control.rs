//! Unix-socket server and client for the local control API
//! ([`twofcast_core::control`]).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, bail};
use twofcast_core::control::{Command, Reply, State, encode, parse};

/// A command for the UI thread and where to send its reply.
pub type Request = (Command, mpsc::Sender<Reply>);

/// Listens on `path` (only the current user may connect) and forwards every
/// command to the UI thread through `tx`.
pub fn serve(path: &Path, tx: async_channel::Sender<Request>) -> anyhow::Result<()> {
    // Replace a socket left over by a previous run, but never another file.
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            bail!("{} exists and is not a socket", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || handle(stream, tx));
            }
        })?;
    Ok(())
}

fn handle(stream: UnixStream, tx: async_channel::Sender<Request>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match parse(&line) {
            Err(e) => Reply::error(e, State::default()),
            Ok(cmd) => {
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx.send_blocking((cmd, reply_tx)).is_err() {
                    return;
                }
                reply_rx
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap_or_else(|_| Reply::error("the app did not answer", State::default()))
            }
        };
        if out.write_all(encode(&reply).as_bytes()).is_err() {
            return;
        }
    }
}

/// Sends one command and returns the reply line (`2fcast --ctl PATH JSON`).
pub fn request(path: &Path, command: &str) -> anyhow::Result<String> {
    let mut stream =
        UnixStream::connect(path).with_context(|| format!("connecting to {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(25)))?;
    stream.write_all(command.trim().as_bytes())?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctl.sock");
        let (tx, rx) = async_channel::unbounded::<Request>();
        serve(&path, tx).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only the user may connect");
        // A fake "UI thread" answering with a recognizable state.
        std::thread::spawn(move || {
            while let Ok((cmd, reply)) = rx.recv_blocking() {
                let state = State {
                    me: format!("{cmd:?}"),
                    ..Default::default()
                };
                let _ = reply.send(Reply::ok(state));
            }
        });
        let line = request(&path, r#"{"cmd":"mute","on":true}"#).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.state.me, "Mute { on: true }");
        let bad: Reply = serde_json::from_str(&request(&path, "nonsense").unwrap()).unwrap();
        assert!(!bad.ok && bad.error.unwrap().starts_with("bad command"));
    }

    #[test]
    fn refuses_to_replace_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("important.txt");
        std::fs::write(&path, "keep me").unwrap();
        let (tx, _rx) = async_channel::unbounded::<Request>();
        assert!(serve(&path, tx).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    }
}
