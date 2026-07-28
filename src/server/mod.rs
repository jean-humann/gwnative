//! Loopback origin for the harness.
//!
//! The client has to run in a secure context (IndexedDB, crypto.subtle) and the
//! harness wants microsecond timers. A custom WKURLSchemeHandler origin gets the
//! secure context but WebKit clamps `performance.now()` there to 1 ms, which is
//! too coarse for the per-frame telemetry. A loopback origin is trustworthy per
//! spec, is not clamped, and — with COOP/COEP — is cross-origin isolated, so
//! SharedArrayBuffer stays available if the client ever wants threads.
//!
//! Bound to 127.0.0.1 on a fixed port, so nothing is reachable off-host. The
//! port has to be fixed rather than ephemeral because it is part of the origin,
//! and WebKit keys IndexedDB by origin: an ephemeral port gives the page a new,
//! empty store on every launch, so nothing the client writes there — skill
//! templates, settings, chat logs — ever survives a restart.
//!
//! What is served splits cleanly in two, and so does the code. [`api`] holds
//! the `__` routes, which are host capabilities and are gated by a token;
//! [`content`] holds the snapshot, the proxy and the files on disk, which are
//! open because the vendored client asks for them itself. This file is the
//! listener, the connection loop and the one dispatch between the two.

use std::io::BufReader;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

mod api;
mod content;

use crate::chunks::ChunkStore;
use crate::diagnostics::Recorder;
use crate::generation;
use crate::http::{MAX_BODY_BYTES, POLICY, Request, policy, read_request, text};
use crate::settings;
use crate::sockets::{self, Registry};
use crate::qos;

/// The origin's port. Any constant would do; this one is unassigned by IANA and
/// sits below macOS's ephemeral floor of 49152, so the kernel will not hand it
/// to somebody else's outbound socket while we are not listening.
///
/// `GWNATIVE_PORT` overrides it, which is also how a second instance gets its
/// own private store rather than fighting over this one.
const PORT: u16 = 38112;

pub struct Loopback {
    pub addr: SocketAddr,
    /// The same store the `__settings` route answers from, so the host can read
    /// what the player chose without a second copy that could disagree.
    pub settings: Arc<settings::Store>,
}

/// Per-request tracing, off unless `GWNATIVE_TRACE_HTTP` is set, matching
/// `GWNATIVE_TRACE_SOCKETS` in [`crate::sockets`].
///
/// A boot issues a couple of hundred range requests and the client keeps asking
/// while it streams content, so writing a line per request is not free: stderr
/// is line-buffered and unbuffered against a terminal, which makes every one of
/// these a synchronous write on the thread serving the read.
fn tracing() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GWNATIVE_TRACE_HTTP").is_some())
}

struct Context {
    root: PathBuf,
    snapshot: Option<Arc<ChunkStore>>,
    sockets: Arc<Registry>,
    recorder: Arc<Recorder>,
    /// The derived client, served in place of the one on disk. See `crate::wasm`
    /// for what it changes and why the base module is kept untouched.
    derived_wasm: Option<PathBuf>,
    settings: Arc<settings::Store>,
    generations: Arc<generation::Store>,
    token: String,
}

/// Serve `root` on 127.0.0.1:<ephemeral> until the process exits.
///
/// `token` gates the credential routes. Loopback is host-wide, not per-user, so
/// without it any process on the machine could ask this server for the saved
/// password — which would make the keychain's own access control decorative.
/// The page receives the token through an injected script, never over this
/// socket, so reading the traffic does not yield it.
pub fn spawn(
    root: PathBuf,
    snapshot: Option<Arc<ChunkStore>>,
    recorder: Arc<Recorder>,
    derived_wasm: Option<PathBuf>,
    settings: Arc<settings::Store>,
    generations: Arc<generation::Store>,
    token: String,
) -> std::io::Result<Loopback> {
    let listener = bind()?;
    let addr = listener.local_addr()?;
    // Before the first response can be written, and only once — a second
    // `spawn` in the same process would be a second origin, which is exactly
    // what `instance` exists to prevent.
    let _ = POLICY.set(policy(addr));
    let context = Arc::new(Context {
        root,
        snapshot,
        sockets: Arc::default(),
        recorder,
        derived_wasm,
        settings: Arc::clone(&settings),
        generations,
        token,
    });

    thread::Builder::new()
        .name("gwnative-loopback".into())
        .spawn(move || {
            // Everything the page is blocked on arrives through this loop, so
            // it and the threads it makes are the interactive path.
            qos::set(qos::Class::UserInitiated);
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let context = Arc::clone(&context);
                // One thread per connection. Snapshot reads block on the chunk
                // store, so they must not share a thread with the page load.
                thread::spawn(move || {
                    qos::set(qos::Class::UserInitiated);
                    let _ = serve(stream, &context);
                });
            }
        })?;

    Ok(Loopback { addr, settings })
}

/// Take [`PORT`], or an ephemeral port if something else already holds it.
///
/// Falling back keeps the app launchable, but it is worth saying out loud: the
/// page will come up on a different origin and so will not find anything it
/// stored on previous launches.
fn bind() -> std::io::Result<TcpListener> {
    let port = std::env::var("GWNATIVE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(PORT);

    match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            note!(
                "[loopback] port {port} is taken, falling back to an ephemeral one; \
                 saved page state will not be found this session"
            );
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        }
        Err(e) => Err(e),
    }
}

/// What to do with the connection once a request has been answered.
enum Flow {
    /// Wait for another request on the same connection.
    Keep,
    Close,
    /// The page upgraded this connection to a socket bridge. It has stopped
    /// being HTTP, so [`serve`] hands both halves to `sockets` and stops.
    Bridge(String),
}

impl Flow {
    /// Whether this connection survives, decided from the request itself.
    ///
    /// Written once rather than per route so that every `respond` in [`api`]
    /// and [`content`] can stay a tail call that says nothing about framing.
    fn after(request: &Request) -> Flow {
        if request.close {
            Flow::Close
        } else {
            Flow::Keep
        }
    }
}

/// How long a kept-alive connection may sit idle before it is reclaimed.
///
/// Keeping connections open is the point of this change, but it means a peer
/// that goes quiet now holds a thread that would previously have been released
/// when the response was written. The client loads continuously while it
/// streams content, so anything this side of a few seconds is generous.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// A write that cannot make progress this long is not going to. Without it a
/// peer that stops reading mid-body parks a thread in `sendto` forever.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn serve(mut stream: TcpStream, context: &Context) -> std::io::Result<()> {
    // A 472-byte snapshot range is the commonest request here, and the peer is
    // on the other end of loopback waiting for it. There is no congestion to
    // protect against, so there is nothing for Nagle to buy by holding the
    // reply back for a full segment.
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        match handle(&mut reader, &mut stream, context)? {
            Flow::Keep => {}
            Flow::Close => return Ok(()),
            Flow::Bridge(destination) => {
                // A game socket is idle for long stretches by design, so the
                // HTTP idle timeout must not follow it across the upgrade.
                stream.set_read_timeout(None)?;
                stream.set_write_timeout(None)?;
                // The reader goes too: anything the peer sent after the request
                // head is already inside its buffer, and a fresh reader over the
                // same socket would drop those bytes.
                sockets::bridge(reader, stream, &destination, &context.sockets);
                return Ok(());
            }
        }
    }
}

/// Read one request and hand it to whichever half of the server owns it.
fn handle(
    reader: &mut BufReader<TcpStream>,
    stream: &mut TcpStream,
    context: &Context,
) -> std::io::Result<Flow> {
    let Some(request) = read_request(reader)? else {
        return Ok(Flow::Close);
    };

    // Closed rather than kept: an oversized body was never read off the socket,
    // so what follows it on a kept-alive connection would be parsed as the next
    // request line.
    if request.content_length > MAX_BODY_BYTES {
        text(stream, 413, "too large")?;
        return Ok(Flow::Close);
    }

    if tracing() {
        note!("[loopback] -> {} /{}", request.method, request.path);
    }

    match api::serve(&request, stream, context)? {
        Some(flow) => Ok(flow),
        None => content::serve(request, stream, context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::TempDir;
    use std::io::Write;

    /// One request, one answer: `(status, body)`.
    ///
    /// A fresh connection per call rather than a kept one, because what this
    /// exercises is the route rather than the keep-alive loop, and a test that
    /// shared a connection would make a failure to answer look like a hang.
    fn request(
        addr: SocketAddr,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).unwrap();
        let auth = match token {
            Some(token) => format!("X-Gwnative-Token: {token}\r\n"),
            None => String::new(),
        };
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             {auth}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut reply = String::new();
        std::io::Read::read_to_string(&mut stream, &mut reply).unwrap();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap_or((reply.as_str(), ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_owned())
    }

    /// The settings route end to end, over a real socket.
    ///
    /// Worth a wire test rather than only unit tests of `settings::patch`: the
    /// token gate, the method table and the file that outlives the process all
    /// live here, and none of them is reachable from a test of the parser. This
    /// is also the only place the gate can be shown to cover `__settings` — it
    /// is written once, for every `__` route, so a route added later inherits
    /// it silently and nothing else would notice if that stopped being true.
    ///
    /// It takes the origin port if that is free, exactly as a second instance
    /// would, and reads the address back from the listener rather than assuming
    /// it — so a test run while the app is up still passes, and the app's own
    /// fallback covers the other order.
    #[test]
    fn the_settings_route_is_gated_typed_and_durable() {
        let temp = TempDir::new("server-settings");
        let dir = temp.0.clone();
        let file = dir.join("settings.json");
        let token = "test-token";
        let loopback = spawn(
            dir.clone(),
            None,
            Recorder::open(dir.join("diagnostics")),
            None,
            Arc::new(settings::Store::open(file.clone())),
            Arc::new(generation::Store::open(dir.join("generations"))),
            token.to_owned(),
        )
        .unwrap();
        let addr = loopback.addr;
        let auth = Some(token);

        // Loopback is host-wide, so an untokened caller is any other process on
        // this machine.
        assert_eq!(request(addr, "GET", "/__settings", None, "").0, 403);

        let (status, body) = request(addr, "GET", "/__settings", auth, "");
        assert_eq!(status, 200);
        assert!(body.contains(r#""dataStrategy":null"#), "{body}");

        let (status, body) = request(
            addr,
            "PUT",
            "/__settings",
            auth,
            r#"{"dataStrategy":"full"}"#,
        );
        assert_eq!(status, 200);
        // The answer is the merged whole, not an acknowledgement: the page has
        // to be able to render the result without a second read.
        assert!(body.contains(r#""dataStrategy":"full""#), "{body}");
        assert!(body.contains(r#""renderScale":2"#), "{body}");

        // A misspelled name is refused rather than quietly ignored.
        assert_eq!(
            request(addr, "PUT", "/__settings", auth, r#"{"renderscale":1}"#).0,
            400,
        );
        assert_eq!(
            request(addr, "DELETE", "/__settings", None, "").0,
            403,
            "the gate comes before the method table",
        );
        assert_eq!(request(addr, "DELETE", "/__settings", auth, "").0, 405);

        // A refused patch must leave what was already saved alone.
        let (_, body) = request(addr, "GET", "/__settings", auth, "");
        assert!(body.contains(r#""renderScale":2"#), "{body}");

        // What a later launch reads is the point of the whole route.
        assert_eq!(
            settings::Store::open(file).get().data_strategy,
            Some(settings::DataStrategy::Full),
        );

        // A name nobody serves is named as such, rather than refused by the
        // static file server as though it might have been a path.
        assert_eq!(request(addr, "GET", "/__nonesuch", auth, "").0, 404);
    }
}
