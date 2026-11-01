//! LAN connection to the other participant.
//!
//! Every instance listens on a TCP port and advertises itself over mDNS
//! (`_twofcast._tcp`).  When two instances see each other, the one with the
//! smaller id dials the other, so exactly one connection is made.  Messages are
//! JSON lines ([`crate::protocol`]); a ping every second detects a dead peer.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::PeerId;
use crate::protocol::{Msg, PROTOCOL_VERSION, decode, encode};
use crate::session::{Peer, peer_was_first};

pub const SERVICE_TYPE: &str = "_twofcast._tcp.local.";
const PING_EVERY: Duration = Duration::from_secs(1);
const DEAD_AFTER: Duration = Duration::from_secs(5);
const DIAL_EVERY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct NetConfig {
    pub id: PeerId,
    pub name: String,
    /// TCP port to listen on (0 = any free port).
    pub port: u16,
    /// Base port the peer should send media to (usually the same as `port`).
    pub base_port: u16,
    pub discovery: bool,
    /// Address to dial directly, for networks without multicast.
    pub static_peer: Option<SocketAddr>,
}

#[derive(Debug)]
pub enum NetEvent {
    Connected(Peer),
    Message(Msg),
    Disconnected,
}

/// The side with the smaller id dials; the other one waits.
pub fn should_dial(me: &str, them: &str) -> bool {
    me < them
}

/// Order in which advertised addresses are tried: typical home LAN ranges
/// first; container bridges (Docker uses 172.17-31.x) and loopback last.
pub fn address_rank(addr: &SocketAddr) -> u8 {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_loopback() => 4,
        IpAddr::V4(ip) if ip.octets()[0] == 192 && ip.octets()[1] == 168 => 0,
        IpAddr::V4(ip) if ip.octets()[0] == 10 => 1,
        IpAddr::V4(ip) if ip.is_private() => 2,
        _ => 3,
    }
}

type Callback = Box<dyn Fn(NetEvent) + Send + Sync>;

struct Conn {
    stream: TcpStream,
    generation: u64,
}

struct Shared {
    cfg: NetConfig,
    on_event: Callback,
    conn: Mutex<Option<Conn>>,
    generation: AtomicU64,
    /// Peers seen via mDNS: id -> addresses.
    discovered: Mutex<HashMap<PeerId, Vec<SocketAddr>>>,
    stop: AtomicBool,
    started: Instant,
}

pub struct Net {
    shared: Arc<Shared>,
    port: u16,
    mdns: Option<mdns_sd::ServiceDaemon>,
}

impl Net {
    pub fn start(
        cfg: NetConfig,
        on_event: impl Fn(NetEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<Net> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.port))?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            cfg,
            on_event: Box::new(on_event),
            conn: Mutex::new(None),
            generation: AtomicU64::new(0),
            discovered: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            started: Instant::now(),
        });

        let s = shared.clone();
        thread::Builder::new()
            .name("net-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if s.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let s = s.clone();
                        thread::spawn(move || {
                            s.handshake(stream);
                        });
                    }
                }
            })?;

        let s = shared.clone();
        thread::Builder::new()
            .name("net-dial".into())
            .spawn(move || s.dial_loop())?;

        let s = shared.clone();
        thread::Builder::new()
            .name("net-ping".into())
            .spawn(move || {
                while !s.stop.load(Ordering::Relaxed) {
                    thread::sleep(PING_EVERY);
                    s.send(&Msg::Ping);
                }
            })?;

        let mdns = if shared.cfg.discovery {
            Some(start_discovery(&shared, port)?)
        } else {
            None
        };
        Ok(Net { shared, port, mdns })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_connected(&self) -> bool {
        self.shared.conn.lock().unwrap().is_some()
    }

    pub fn send(&self, msg: &Msg) {
        self.shared.send(msg);
    }

    /// Closes the current connection (the peer will reconnect).
    pub fn drop_connection(&self) {
        if let Some(c) = self.shared.conn.lock().unwrap().as_ref() {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.drop_connection();
        if let Some(d) = self.mdns.take() {
            let _ = d.shutdown();
        }
        // Wake up the accept loop so it can exit.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_millis(200),
        );
    }
}

fn start_discovery(shared: &Arc<Shared>, port: u16) -> anyhow::Result<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let id = &shared.cfg.id;
    let props = [("id", id.as_str()), ("name", shared.cfg.name.as_str())];
    let info = mdns_sd::ServiceInfo::new(
        SERVICE_TYPE,
        id,
        &format!("twofcast-{id}.local."),
        "",
        port,
        &props[..],
    )?
    .enable_addr_auto();
    daemon.register(info)?;
    let events = daemon.browse(SERVICE_TYPE)?;
    let s = shared.clone();
    thread::Builder::new()
        .name("net-mdns".into())
        .spawn(move || {
            while let Ok(ev) = events.recv() {
                if let mdns_sd::ServiceEvent::ServiceResolved(info) = ev {
                    let Some(peer_id) = info.get_property_val_str("id") else {
                        continue;
                    };
                    if peer_id == s.cfg.id {
                        continue;
                    }
                    let port = info.get_port();
                    let mut addrs: Vec<SocketAddr> = info
                        .get_addresses_v4()
                        .into_iter()
                        .map(|ip| SocketAddr::from((ip, port)))
                        .collect();
                    addrs.sort();
                    s.discovered
                        .lock()
                        .unwrap()
                        .insert(peer_id.to_string(), addrs);
                }
            }
        })?;
    Ok(daemon)
}

impl Shared {
    fn emit(&self, ev: NetEvent) {
        if !self.stop.load(Ordering::Relaxed) {
            (self.on_event)(ev);
        }
    }

    fn send(&self, msg: &Msg) {
        let mut guard = self.conn.lock().unwrap();
        if let Some(c) = guard.as_mut()
            && c.stream.write_all(encode(msg).as_bytes()).is_err()
        {
            // The reader thread notices the broken socket and reports it.
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn dial_loop(self: Arc<Self>) {
        while !self.stop.load(Ordering::Relaxed) {
            if self.conn.lock().unwrap().is_none() {
                let mut targets: Vec<SocketAddr> = self.cfg.static_peer.into_iter().collect();
                for (id, addrs) in self.discovered.lock().unwrap().iter() {
                    if should_dial(&self.cfg.id, id) {
                        targets.extend(addrs);
                    }
                }
                targets.sort_by_key(address_rank);
                for addr in targets {
                    // An address may lead back to ourselves (e.g. both machines
                    // have the same Docker bridge address); then try the next.
                    if let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
                        && self.clone().handshake(stream)
                    {
                        break;
                    }
                }
            }
            thread::sleep(DIAL_EVERY);
        }
    }

    /// Exchanges `Hello`s, then (if no connection exists yet) becomes the
    /// connection and reads messages until it breaks.  Returns false if the
    /// connection was never established.
    fn handshake(self: Arc<Self>, mut stream: TcpStream) -> bool {
        if self.conn.lock().unwrap().is_some() {
            return false; // Only one peer at a time.
        }
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(DEAD_AFTER));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let my_uptime = self.started.elapsed().as_millis() as u64;
        let hello = Msg::Hello {
            id: self.cfg.id.clone(),
            name: self.cfg.name.clone(),
            base_port: self.cfg.base_port,
            version: PROTOCOL_VERSION,
            uptime_ms: my_uptime,
        };
        if stream.write_all(encode(&hello).as_bytes()).is_err() {
            return false;
        }
        let Ok(read_half) = stream.try_clone() else {
            return false;
        };
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return false;
        }
        let Ok(Msg::Hello {
            id,
            name,
            base_port,
            version,
            uptime_ms,
        }) = decode(&line)
        else {
            return false;
        };
        if version != PROTOCOL_VERSION || id == self.cfg.id {
            return false;
        }
        let ip: IpAddr = match stream.peer_addr() {
            Ok(a) => a.ip(),
            Err(_) => return false,
        };
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut guard = self.conn.lock().unwrap();
            if guard.is_some() {
                return false; // Already connected; this one is a duplicate.
            }
            let Ok(write_half) = stream.try_clone() else {
                return false;
            };
            *guard = Some(Conn {
                stream: write_half,
                generation,
            });
            // Emit while holding the lock so no message can overtake `Connected`.
            let was_first = peer_was_first(my_uptime, &self.cfg.id, uptime_ms, &id);
            self.emit(NetEvent::Connected(Peer {
                id,
                name,
                ip,
                base_port,
                was_first,
            }));
        }

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => match decode(&line) {
                    Ok(Msg::Ping) => {}
                    Ok(msg) => self.emit(NetEvent::Message(msg)),
                    Err(_) => {} // Ignore what we don't understand.
                },
            }
        }

        let _ = stream.shutdown(std::net::Shutdown::Both);
        let mut guard = self.conn.lock().unwrap();
        if guard.as_ref().is_some_and(|c| c.generation == generation) {
            *guard = None;
            self.emit(NetEvent::Disconnected);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn cfg(id: &str, static_peer: Option<SocketAddr>, discovery: bool) -> NetConfig {
        NetConfig {
            id: id.into(),
            name: id.to_uppercase(),
            port: 0,
            base_port: 47100,
            discovery,
            static_peer,
        }
    }

    fn start(c: NetConfig) -> (Net, mpsc::Receiver<NetEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let net = Net::start(c, move |e| {
            let _ = tx.lock().unwrap().send(e);
        })
        .unwrap();
        (net, rx)
    }

    fn next(rx: &mpsc::Receiver<NetEvent>, secs: u64) -> NetEvent {
        rx.recv_timeout(Duration::from_secs(secs))
            .expect("expected a network event")
    }

    fn local(port: u16) -> Option<SocketAddr> {
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    #[test]
    fn dial_rule_is_antisymmetric() {
        assert!(should_dial("a", "b"));
        assert!(!should_dial("b", "a"));
        assert!(!should_dial("a", "a"));
    }

    #[test]
    fn lan_addresses_are_tried_first() {
        let mut v: Vec<SocketAddr> = [
            "127.0.0.1:1",
            "172.17.0.1:1",
            "192.168.1.20:1",
            "10.0.0.5:1",
        ]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
        v.sort_by_key(address_rank);
        assert_eq!(v[0].to_string(), "192.168.1.20:1");
        assert_eq!(v[1].to_string(), "10.0.0.5:1");
        assert_eq!(v[3].to_string(), "127.0.0.1:1");
    }

    #[test]
    fn skips_addresses_that_lead_back_to_ourselves() {
        let (b, rx_b) = start(cfg("b", None, false));
        let (a, rx_a) = start(cfg("a", None, false));
        // Pretend mDNS gave "b" two addresses: the first one is really "a".
        a.shared.discovered.lock().unwrap().insert(
            "b".into(),
            vec![local(a.port()).unwrap(), local(b.port()).unwrap()],
        );
        let NetEvent::Connected(peer) = next(&rx_a, 8) else {
            panic!()
        };
        assert_eq!(peer.id, "b");
        assert!(matches!(next(&rx_b, 5), NetEvent::Connected(_)));
    }

    #[test]
    fn connects_exchanges_and_detects_disconnect() {
        let (b, rx_b) = start(cfg("b", None, false));
        let (a, rx_a) = start(cfg("a", local(b.port()), false));

        let NetEvent::Connected(peer) = next(&rx_a, 5) else {
            panic!()
        };
        assert_eq!(
            (peer.id.as_str(), peer.name.as_str(), peer.base_port),
            ("b", "B", 47100)
        );
        let NetEvent::Connected(peer) = next(&rx_b, 5) else {
            panic!()
        };
        assert_eq!(peer.id, "a");
        assert!(a.is_connected() && b.is_connected());

        a.send(&Msg::Recording {
            active: true,
            elapsed_ms: 0,
        });
        match next(&rx_b, 5) {
            NetEvent::Message(Msg::Recording {
                active: true,
                elapsed_ms: 0,
            }) => {}
            e => panic!("{e:?}"),
        }
        b.send(&Msg::Recording {
            active: false,
            elapsed_ms: 0,
        });
        assert!(matches!(
            next(&rx_a, 5),
            NetEvent::Message(Msg::Recording {
                active: false,
                elapsed_ms: 0
            })
        ));

        drop(b);
        assert!(matches!(next(&rx_a, 8), NetEvent::Disconnected));
        assert!(!a.is_connected());
    }

    #[test]
    fn reconnects_after_connection_drop() {
        let (b, rx_b) = start(cfg("b", None, false));
        let (a, rx_a) = start(cfg("a", local(b.port()), false));
        assert!(matches!(next(&rx_a, 5), NetEvent::Connected(_)));
        assert!(matches!(next(&rx_b, 5), NetEvent::Connected(_)));
        a.drop_connection();
        assert!(matches!(next(&rx_a, 5), NetEvent::Disconnected));
        assert!(
            matches!(next(&rx_a, 5), NetEvent::Connected(_)),
            "redials automatically"
        );
    }

    #[test]
    fn refuses_second_peer_and_self() {
        let (b, rx_b) = start(cfg("b", None, false));
        let (_a, rx_a) = start(cfg("a", local(b.port()), false));
        assert!(matches!(next(&rx_b, 5), NetEvent::Connected(_)));
        assert!(matches!(next(&rx_a, 5), NetEvent::Connected(_)));
        let (_c, rx_c) = start(cfg("c", local(b.port()), false));
        assert!(
            rx_c.recv_timeout(Duration::from_secs(2)).is_err(),
            "third participant is not accepted"
        );
        let (me, rx_me) = start(cfg("x", None, false));
        let (_me2, _) = start(cfg("x", local(me.port()), false));
        assert!(
            rx_me.recv_timeout(Duration::from_secs(2)).is_err(),
            "same id is refused"
        );
    }

    #[test]
    fn ignores_garbage_lines() {
        let (b, rx_b) = start(cfg("b", None, false));
        let mut raw = TcpStream::connect(local(b.port()).unwrap()).unwrap();
        let hello = Msg::Hello {
            id: "a".into(),
            name: "A".into(),
            base_port: 1,
            version: PROTOCOL_VERSION,
            uptime_ms: 0,
        };
        raw.write_all(encode(&hello).as_bytes()).unwrap();
        raw.write_all(b"{garbage}\n").unwrap();
        raw.write_all(
            encode(&Msg::Recording {
                active: true,
                elapsed_ms: 0,
            })
            .as_bytes(),
        )
        .unwrap();
        assert!(matches!(next(&rx_b, 5), NetEvent::Connected(_)));
        assert!(matches!(
            next(&rx_b, 5),
            NetEvent::Message(Msg::Recording {
                active: true,
                elapsed_ms: 0
            })
        ));
    }

    /// Needs multicast on the network interfaces; run with `--ignored`.
    #[test]
    #[ignore]
    fn discovers_over_mdns() {
        let (_b, rx_b) = start(cfg("mdns-test-b", None, true));
        let (_a, rx_a) = start(cfg("mdns-test-a", None, true));
        assert!(matches!(next(&rx_a, 20), NetEvent::Connected(_)));
        assert!(matches!(next(&rx_b, 20), NetEvent::Connected(_)));
    }
}
