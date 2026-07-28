//! Game TCP sockets, bridged to the page over one WebSocket each.
//!
//! There is deliberately no queue in either direction. The Electron host it
//! replaces buffers outbound payloads and has to defend that buffer with
//! per-socket and per-owner byte caps; here each direction is a blocking copy
//! between two sockets on its own thread, so the kernel's own send and receive
//! windows are the backpressure. Nothing accumulates in the host, so there is
//! no bound to get wrong.

use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

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

#[derive(Default)]
pub struct Registry {
    open: AtomicUsize,
}

impl Registry {
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

fn escape(text: &str) -> String {
    text.replace('\\', r"\\").replace('"', "\\\"")
}

/// Per-packet tracing, off unless `GWNATIVE_TRACE_SOCKETS` is set.
///
/// `=hex` additionally prints the head of each frame, which is what tells a
/// heartbeat apart from a request that went unanswered. It stops at
/// [`TRACE_HEAD`] bytes: the login exchange carries the account password, and a
/// trace is exactly the kind of thing a bug report carries off the machine.
/// A Guild Wars packet header fits well inside that, so the cap costs nothing.
const TRACE_HEAD: usize = 16;

#[derive(Clone, Copy, PartialEq)]
enum Trace {
    Off,
    Sizes,
    Hex,
}

fn tracing() -> Trace {
    static MODE: OnceLock<Trace> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("GWNATIVE_TRACE_SOCKETS") {
        Err(_) => Trace::Off,
        Ok(value) if value.eq_ignore_ascii_case("hex") => Trace::Hex,
        Ok(_) => Trace::Sizes,
    })
}

/// One trace line: direction, length, and — in hex mode — the capped head.
fn trace(destination: &str, direction: &str, data: &[u8]) {
    match tracing() {
        Trace::Off => {}
        Trace::Sizes => note!("[socket] {destination}: {direction} {}", data.len()),
        Trace::Hex => {
            let head = &data[..data.len().min(TRACE_HEAD)];
            let hex: String = head.iter().map(|b| format!("{b:02x}")).collect();
            let elided = if data.len() > head.len() { "…" } else { "" };
            note!(
                "[socket] {destination}: {direction} {} {hex}{elided}",
                data.len()
            );
        }
    }
}

/// Bridge an upgraded WebSocket to `destination` until either side closes.
///
/// The dial happens after the upgrade, so success cannot be signalled by the
/// WebSocket handshake itself; an `open` or `error` text frame carries it
/// instead. Every later frame is binary and maps one-to-one onto TCP payload.
pub fn bridge(
    mut reader: BufReader<TcpStream>,
    page: TcpStream,
    destination: &str,
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
        let _ = sink.send(
            TEXT,
            br#"{"type":"error","message":"too many open sockets"}"#,
        );
        sink.close();
        return;
    };

    let game = match net::connect(destination) {
        Ok(stream) => stream,
        Err(e) => {
            note!("[socket] {destination}: {e}");
            let _ = sink.send(
                TEXT,
                format!(
                    r#"{{"type":"error","message":"{}"}}"#,
                    escape(&e.to_string())
                )
                .as_bytes(),
            );
            sink.close();
            return;
        }
    };
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
        let destination = destination.to_owned();
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
                        trace(&destination, "down", &buffer[..n]);
                    }
                    _ => break,
                }
            }
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
            Ok(Some(Message::Binary(data))) => {
                sent += data.len();
                trace(destination, "up", &data);
                if game.write_all(&data).is_err() {
                    break;
                }
            }
            // Control frames travel host→page only. One arriving the other way
            // means the two sides disagree about the protocol, so say so rather
            // than dropping it into the TCP stream as if it were packet data.
            Ok(Some(Message::Text(text))) => {
                note!("[socket] {destination}: unexpected control frame from page: {text}");
            }
            Ok(Some(Message::Close)) | Ok(None) => break,
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
