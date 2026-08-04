//! Game TCP sockets, bridged to the page over one WebSocket each.
//!
//! There is deliberately no queue in either direction. Each direction is a
//! blocking copy between two sockets on its own thread, so the kernel's send and
//! receive windows provide backpressure. Nothing accumulates in the host, so
//! there is no application buffer or byte cap to get wrong.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, TcpStream};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::generation;
use crate::http::WipingReader;
use crate::net;
use crate::qos;
use crate::ws::{self, BINARY, Message, Sink, TEXT};

/// Read size for the game→page direction. Guild Wars packets are far smaller;
/// this only caps how much one read can hand over at once.
const READ_BUFFER: usize = 64 * 1024;

/// The client opens a handful of sockets. A cap this far above normal use only
/// catches a runaway loop, which would otherwise spawn threads until the
/// process died.
const MAX_SOCKETS: usize = 64;

pub struct Registry {
    open: AtomicUsize,
    resolved_gameplay: Mutex<Vec<IpAddr>>,
    gameplay_observed: Mutex<Vec<generation::LaunchIdentity>>,
    generations: Arc<generation::Store>,
    pending_proofs: Arc<PendingProofs>,
}

#[derive(Default)]
struct PendingProofs {
    count: Mutex<usize>,
    done: Condvar,
}

impl PendingProofs {
    fn begin(self: &Arc<Self>) -> PendingProof {
        *self.count.lock().unwrap() += 1;
        PendingProof(Arc::clone(self))
    }

    fn wait(&self, timeout: Duration) -> bool {
        let count = self.count.lock().unwrap();
        let (count, _) = self
            .done
            .wait_timeout_while(count, timeout, |count| *count != 0)
            .unwrap();
        *count == 0
    }
}

struct PendingProof(Arc<PendingProofs>);

impl Drop for PendingProof {
    fn drop(&mut self) {
        let mut count = self.0.count.lock().unwrap();
        *count -= 1;
        self.0.done.notify_all();
    }
}

impl Registry {
    pub fn new(generations: Arc<generation::Store>) -> Self {
        Self {
            open: AtomicUsize::new(0),
            resolved_gameplay: Mutex::new(Vec::new()),
            gameplay_observed: Mutex::new(Vec::new()),
            generations,
            pending_proofs: Arc::new(PendingProofs::default()),
        }
    }

    /// Remember an address returned by this process's allowlisted ArenaNet DNS
    /// resolver. The client normally discards the name and later dials only
    /// the dotted quad, so this host-owned provenance is what distinguishes a
    /// real game endpoint from an arbitrary public server on port 6112.
    pub fn resolved_allowed_name(&self, address: Ipv4Addr) {
        let address = IpAddr::V4(address);
        let mut resolved = self.resolved_gameplay.lock().unwrap();
        if !resolved.contains(&address) {
            resolved.push(address);
            if resolved.len() > 64 {
                resolved.remove(0);
            }
        }
    }

    /// Bind a page-provided identity to the exact launch active when its socket
    /// handshake begins. A missing, malformed, or stale identity never blocks
    /// the socket; it merely has no authority to prove a different session.
    pub fn bind_gameplay(
        &self,
        destination: &str,
        claimed: &generation::LaunchClaim,
    ) -> Option<generation::LaunchIdentity> {
        let permitted = match net::gameplay_peer(destination)? {
            net::GameplayPeer::AllowedName => true,
            net::GameplayPeer::Address(address) => {
                self.resolved_gameplay.lock().unwrap().contains(&address)
            }
        };
        if !permitted {
            return None;
        }
        self.generations.resolve_launch_claim(claimed)
    }

    fn settle_gameplay_proof(self: &Arc<Self>, launch: generation::LaunchIdentity) {
        let observed = self
            .gameplay_observed
            .lock()
            .unwrap()
            .iter()
            .any(|candidate| candidate == &launch);
        if !observed {
            return;
        }
        let pending = self.pending_proofs.begin();
        let registry = Arc::clone(self);
        thread::spawn(move || {
            let _pending = pending;
            if let Err(error) = registry.generations.prove_gameplay(&launch) {
                note!("[generation] could not record the gameplay milestone: {error}");
            }
        });
    }

    /// The first permitted ArenaNet TCP connection is a host-owned session
    /// milestone. It can arrive before the renderer's first-frame request, so
    /// remember it and let either event settle the proof once both happened.
    pub fn gameplay_connected(self: &Arc<Self>, launch: generation::LaunchIdentity) {
        let mut observed = self.gameplay_observed.lock().unwrap();
        if !observed.contains(&launch) {
            observed.push(launch.clone());
            if observed.len() > 8 {
                observed.remove(0);
            }
        }
        drop(observed);
        self.settle_gameplay_proof(launch);
    }

    pub fn first_frame_proven(self: &Arc<Self>, launch: &generation::LaunchIdentity) {
        self.settle_gameplay_proof(launch.clone());
    }

    /// Wait only for proof writes already dispatched. The page races this with
    /// its existing quit deadline, so a wedged filesystem still cannot make the
    /// application refuse to close.
    pub fn flush_proofs(&self, timeout: Duration) -> bool {
        self.pending_proofs.wait(timeout)
    }

    /// Take a slot, returning a guard that releases it on drop.
    fn claim(self: &Arc<Self>) -> Option<Slot> {
        let mut current = self.open.load(Ordering::Relaxed);
        loop {
            if current >= MAX_SOCKETS {
                return None;
            }
            match self.open.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Slot(Arc::clone(self))),
                Err(actual) => current = actual,
            }
        }
    }
}

struct Slot(Arc<Registry>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One `error` frame, built by the JSON encoder rather than spelled out here.
///
/// The message is not ours: [`net::connect`] names the destination back in
/// `NameNotAllowed` and `BadDestination`, and that destination is a query-string
/// parameter the page chose, percent-decoded — so it holds whatever a query
/// string can hold, control characters included. Escaping `\` and `"` by hand
/// and stopping there leaves a raw newline sitting inside a JSON string, which
/// is not JSON at all: `JSON.parse` in the page throws, and the reason the
/// socket failed is lost at the one moment it is worth having. `serde_json`
/// already builds every other JSON body in this crate and knows the rest of the
/// escapes.
fn error_frame(message: &str) -> Option<String> {
    String::from_utf8(crate::log::redact_json(
        &serde_json::json!({ "type": "error", "message": message }),
    )?)
    .ok()
}

fn send_error(sink: &Sink, message: &str) {
    let Some(mut frame) = error_frame(message) else {
        return;
    };
    if let Some(_lease) = crate::log::admit_untrusted(frame.as_bytes()) {
        let _ = sink.send(TEXT, frame.as_bytes());
    }
    crate::log::wipe_string(&mut frame);
}

fn wipe_error(error: &mut net::NetError) {
    match error {
        net::NetError::BadDestination(value)
        | net::NetError::NameNotAllowed(value)
        | net::NetError::Resolve(value)
        | net::NetError::Connect(value) => crate::log::wipe_string(value),
        net::NetError::PortNotAllowed(_) | net::NetError::NotPublicUnicast(_) => {}
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Trace {
    Off,
    Sizes,
}

static TRACE_OVERRIDE: AtomicU8 = AtomicU8::new(0);

pub fn enable_tracing() {
    TRACE_OVERRIDE.store(1, Ordering::Relaxed);
}

fn tracing() -> Trace {
    static MODE: OnceLock<Trace> = OnceLock::new();
    if TRACE_OVERRIDE.load(Ordering::Relaxed) != 0 {
        return Trace::Sizes;
    }
    *MODE.get_or_init(|| {
        if std::env::var_os("GWNATIVE_TRACE_SOCKETS").is_some() {
            Trace::Sizes
        } else {
            Trace::Off
        }
    })
}

/// One trace line: direction, destination class, and length. Packet bytes are
/// never diagnostics: the login exchange can carry the account password.
fn trace(direction: &str, data: &[u8]) {
    match tracing() {
        Trace::Off => {}
        Trace::Sizes => note!("[socket] game: {direction} {}", data.len()),
    }
}

/// Bridge an upgraded WebSocket to `destination` until either side closes.
///
/// The dial happens after the upgrade, so success cannot be signalled by the
/// WebSocket handshake itself; an `open` or `error` text frame carries it
/// instead. Every later frame is binary and maps one-to-one onto TCP payload.
pub fn bridge(
    mut reader: WipingReader<TcpStream>,
    page: TcpStream,
    destination: &str,
    gameplay_launch: Option<generation::LaunchIdentity>,
    registry: &Arc<Registry>,
) {
    let sink = Sink::new(match page.try_clone() {
        Ok(clone) => clone,
        Err(e) => {
            note!("[socket] {destination}: cannot split page socket: {e}");
            return;
        }
    });

    let Some(_slot) = registry.claim() else {
        send_error(&sink, "too many open sockets");
        sink.close();
        return;
    };

    let Some(destination_lease) = crate::log::admit_untrusted(destination.as_bytes()) else {
        send_error(&sink, "connection refused by host policy");
        sink.close();
        return;
    };
    let connected = net::connect(destination);
    drop(destination_lease);
    let game = match connected {
        Ok(stream) => stream,
        Err(mut e) => {
            note!("[socket] {destination}: {e}");
            send_error(&sink, "connection refused by host policy");
            wipe_error(&mut e);
            sink.close();
            return;
        }
    };
    if let Some(launch) = gameplay_launch {
        registry.gameplay_connected(launch);
    }
    note!("[socket] {destination}: connected");
    if sink.send(TEXT, br#"{"type":"open"}"#).is_err() {
        return;
    }
    let downstream = Arc::new(AtomicUsize::new(0));

    // game → page, on its own thread so both directions can block.
    let uplink = {
        let sink = sink.clone();
        let mut game = match game.try_clone() {
            Ok(clone) => clone,
            Err(e) => {
                note!("[socket] {destination}: cannot split game socket: {e}");
                sink.close();
                return;
            }
        };
        let counter = Arc::clone(&downstream);
        thread::spawn(move || {
            // This carries the game's own traffic to and from ArenaNet; a frame
            // of added latency here is a frame the player waits.
            qos::set(qos::Class::UserInitiated);
            let mut buffer = vec![0u8; READ_BUFFER];
            loop {
                match game.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) if sink.send(BINARY, &buffer[..n]).is_ok() => {
                        counter.fetch_add(n, Ordering::Relaxed);
                        trace("down", &buffer[..n]);
                        crate::log::wipe(&mut buffer[..n]);
                    }
                    Ok(n) => {
                        crate::log::wipe(&mut buffer[..n]);
                        break;
                    }
                    Err(_) => break,
                }
            }
            crate::log::wipe(&mut buffer);
            // Whichever direction ends first tears down the pair, so the peer
            // thread's blocking read returns instead of leaking a thread.
            sink.close();
            let _ = game.shutdown(Shutdown::Both);
        })
    };

    // page → game, on this thread.
    let mut game = game;
    let mut sent = 0usize;
    loop {
        match ws::read_message(&mut reader, &sink) {
            Ok(Some(Message::Binary(mut data))) => {
                sent += data.len();
                trace("up", &data);
                let written = game.write_all(&data);
                crate::log::wipe(&mut data);
                if written.is_err() {
                    break;
                }
            }
            // Control frames travel host→page only. One arriving the other way
            // means the two sides disagree about the protocol, so say so rather
            // than dropping it into the TCP stream as if it were packet data.
            Ok(Some(Message::Text(mut text))) => {
                note!(
                    "[socket] game: unexpected control frame from page ({} bytes)",
                    text.len()
                );
                crate::log::wipe_string(&mut text);
            }
            Ok(Some(Message::Close) | None) => break,
            Err(e) => {
                note!("[socket] {destination}: page read failed: {e}");
                break;
            }
        }
    }

    let _ = game.shutdown(Shutdown::Both);
    sink.close();
    let _ = uplink.join();
    // Safe to drop the page connection now: the read loop above has finished,
    // so there is nothing unread left to turn this into an RST.
    let _ = page.shutdown(Shutdown::Both);
    note!(
        "[socket] {destination}: closed after {sent} bytes up, {} down",
        downstream.load(Ordering::Relaxed)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;

    #[test]
    fn clean_quit_waits_for_a_pending_proof_but_remains_bounded() {
        let pending = Arc::new(PendingProofs::default());
        let job = pending.begin();
        let waiter = Arc::clone(&pending);
        let (sent, received) = std::sync::mpsc::channel();
        thread::spawn(move || {
            sent.send(waiter.wait(Duration::from_secs(1))).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
        drop(job);
        assert!(received.recv_timeout(Duration::from_secs(1)).unwrap());

        let job = pending.begin();
        assert!(!pending.wait(Duration::ZERO));
        drop(job);
    }

    #[test]
    fn gameplay_binding_requires_host_owned_arenanet_provenance() {
        const NAMES: [&str; 2] = ["Gw.jspi.js", "Gw.jspi.wasm"];
        const NONCE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let temp = TempDir::new("socket-gameplay-class");
        let root = temp.0.join("web");
        std::fs::create_dir_all(&root).unwrap();
        for name in NAMES {
            std::fs::write(root.join(name), name).unwrap();
        }
        std::fs::write(temp.0.join("manifest.cache"), b"manifest").unwrap();
        let generations = Arc::new(generation::Store::open(temp.0.join("generations")));
        assert!(generations.record("current", &root, &NAMES));
        let launch = generations
            .record_attempt("jspi", None, false, NONCE)
            .unwrap();
        let claim = generation::LaunchClaim {
            runtime: launch.runtime.clone(),
            build: None,
            transformed: false,
            nonce: launch.nonce.clone(),
        };
        let registry = Registry::new(generations);

        assert_eq!(registry.bind_gameplay("1.2.3.4:6112", &claim), None);
        assert_eq!(registry.bind_gameplay("8.8.8.8:6112", &claim), None);
        assert_eq!(
            registry.bind_gameplay("auth.arenanetworks.com:6112", &claim),
            Some(launch.clone())
        );
        registry.resolved_allowed_name("1.2.3.4".parse().unwrap());
        assert_eq!(registry.bind_gameplay("1.2.3.4:6112", &claim), Some(launch));
        assert_eq!(
            registry.bind_gameplay("www.guildwars.com:443", &claim),
            None
        );
    }

    /// The page parses this frame to find out why its socket never opened, so a
    /// destination that makes the frame unparseable costs it the diagnosis —
    /// `?to=` is where the destination comes from, and a query string carries
    /// bytes a hand-written escaper does not cover.
    #[test]
    fn an_error_frame_is_json_whatever_the_message_holds() {
        for message in [
            "a\nb is not an ArenaNet name",
            "quote \" and backslash \\ together",
            "tab\tand a bell \u{7}",
            "malformed destination: \u{1}\u{2}\u{3}",
            "resolve failed: naïve.example.com",
        ] {
            let frame = error_frame(message).unwrap();
            let parsed: serde_json::Value =
                serde_json::from_str(&frame).unwrap_or_else(|e| panic!("{frame:?}: {e}"));
            assert_eq!(parsed["type"], "error");
            // Round-tripped, not merely well-formed: an escaper that dropped the
            // offending bytes would also parse.
            assert_eq!(parsed["message"], message);
        }
    }

    #[test]
    fn protected_socket_destinations_are_refused_before_dialing() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (page, _) = listener.accept().unwrap();
        let reader = WipingReader::new(page.try_clone().unwrap());
        let temp = TempDir::new("socket-protected-destination");
        let registry = Arc::new(Registry::new(Arc::new(generation::Store::open(
            temp.0.join("generations"),
        ))));
        let secret = "socket-prefix-canary:6112";
        let _registration = crate::log::register(&[secret]).unwrap();

        bridge(reader, page, secret, None, &registry);
        let mut wire = Vec::new();
        client.read_to_end(&mut wire).unwrap();
        assert!(
            !wire
                .windows(secret.len())
                .any(|part| part == secret.as_bytes()),
            "protected destination reached a socket response"
        );
        crate::log::wipe(&mut wire);
    }
}
