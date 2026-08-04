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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

mod api;
mod content;

use crate::chunks::ChunkStore;
use crate::diagnostics::Recorder;
use crate::game_api;
use crate::generation;
use crate::http::{MAX_BODY_BYTES, POLICY, Request, policy, read_request, text};
use crate::qos;
use crate::relaunch;
use crate::settings;
use crate::sockets::{self, Registry};
use crate::wasm;

/// The origin's port. Any constant would do; this one is unassigned by IANA and
/// sits below macOS's ephemeral floor of 49152, so the kernel will not hand it
/// to somebody else's outbound socket while we are not listening.
///
/// `GWNATIVE_PORT` overrides it, which is also how a second instance gets its
/// own private store rather than fighting over this one.
#[cfg(test)]
const PORT: u16 = 38112;

/// How long a relaunched app waits for its predecessor to close the port, and
/// how often it looks. Short: the wait is for a descriptor already on its way
/// out, not for a process, and anything longer than this means something else
/// on the machine has the port and no amount of waiting will help.
const PORT_PATIENCE: Duration = Duration::from_millis(500);
const PORT_POLL: Duration = Duration::from_millis(10);

pub struct Loopback {
    pub addr: SocketAddr,
    /// The same store the `__settings` route answers from, so the host can read
    /// what the player chose without a second copy that could disagree.
    pub settings: Arc<settings::ScopedStore>,
    /// The same recorder `__report` and `__diag` write into, for the same
    /// reason: the menu's Mark a Slowdown has to land in the file the page's
    /// own counters are landing in, or the two describe different sessions.
    pub recorder: Arc<Recorder>,
}

/// Per-request tracing, off unless `GWNATIVE_TRACE_HTTP` is set, matching
/// `GWNATIVE_TRACE_SOCKETS` in [`crate::sockets`].
///
/// A boot issues a couple of hundred range requests and the client keeps asking
/// while it streams content, so writing a line per request is not free: stderr
/// is line-buffered and unbuffered against a terminal, which makes every one of
/// these a synchronous write on the thread serving the read.
static TRACE_REQUESTS: AtomicBool = AtomicBool::new(false);

pub fn enable_tracing() {
    TRACE_REQUESTS.store(true, Ordering::Relaxed);
}

fn tracing() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    TRACE_REQUESTS.load(Ordering::Relaxed)
        || *ON.get_or_init(|| std::env::var_os("GWNATIVE_TRACE_HTTP").is_some())
}

struct Context {
    /// Official client artifacts only; never a mutable browser shell.
    root: PathBuf,
    /// One immutable, inventory-verified shell revision.
    shell_root: PathBuf,
    snapshot: Option<Arc<ChunkStore>>,
    sockets: Arc<Registry>,
    recorder: Arc<Recorder>,
    /// The derived client, served in place of the one on disk. See `crate::wasm`
    /// for what it changes and why the base module is kept untouched.
    derived_wasm: wasm::DerivedModules,
    settings: Arc<settings::ScopedStore>,
    generations: Arc<generation::Store>,
    tokens: CapabilityTokens,
    /// Profile-specific Keychain account name. The service remains stable so
    /// existing default-profile credentials keep working.
    credential_account: String,
    game_api: Arc<game_api::Hub>,
}

/// A browser uses only a small connection pool. This ceiling leaves ample room
/// for its HTTP and WebSocket traffic while preventing a local process from
/// turning the one-thread-per-connection design into an unbounded thread farm.
const MAX_CONNECTIONS: usize = 256;

struct Connection(Arc<AtomicUsize>);

impl Connection {
    fn claim(active: &Arc<AtomicUsize>) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_CONNECTIONS).then_some(count + 1)
            })
            .ok()?;
        Some(Self(Arc::clone(active)))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Serve `root` on 127.0.0.1:<ephemeral> until the process exits.
///
/// Capability tokens gate host routes. Loopback is host-wide, not per-user, so
/// the browser's administrative authority, an external reader, and the page's
/// state publisher must not share one bearer credential.
#[derive(Clone)]
pub struct CapabilityTokens {
    /// Full page authority, including credentials and process control.
    pub browser: String,
    /// Read-only access to the deliberately narrow public game-state schema.
    pub game_reader: String,
    /// Write-only access for the in-page state publisher.
    pub game_publisher: String,
}

pub struct Config {
    pub root: PathBuf,
    pub shell_root: PathBuf,
    pub snapshot: Option<Arc<ChunkStore>>,
    pub recorder: Arc<Recorder>,
    pub derived_wasm: wasm::DerivedModules,
    pub settings: Arc<settings::ScopedStore>,
    pub generations: Arc<generation::Store>,
    pub tokens: CapabilityTokens,
    pub port: u16,
    pub credential_account: String,
}

pub fn spawn(config: Config) -> std::io::Result<Loopback> {
    let Config {
        root,
        shell_root,
        snapshot,
        recorder,
        derived_wasm,
        settings,
        generations,
        tokens,
        port,
        credential_account,
    } = config;
    let listener = bind(port)?;
    let addr = listener.local_addr()?;
    // Before the first response can be written, and only once — a second
    // `spawn` in the same process would be a second origin, which is exactly
    // what `instance` exists to prevent.
    let _ = POLICY.set(policy(addr));
    let sockets = Arc::new(Registry::new(Arc::clone(&generations)));
    let context = Arc::new(Context {
        root,
        shell_root,
        snapshot,
        sockets,
        recorder: Arc::clone(&recorder),
        derived_wasm,
        settings: Arc::clone(&settings),
        generations,
        tokens,
        credential_account,
        game_api: Arc::default(),
    });
    let active = Arc::new(AtomicUsize::new(0));

    thread::Builder::new()
        .name("gwnative-loopback".into())
        .spawn(move || {
            // Everything the page is blocked on arrives through this loop, so
            // it and the threads it makes are the interactive path.
            qos::set(qos::Class::UserInitiated);
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let Some(connection) = Connection::claim(&active) else {
                    continue;
                };
                let context = Arc::clone(&context);
                // One thread per connection. Snapshot reads block on the chunk
                // store, so they must not share a thread with the page load. The
                // connection guard also bounds how many such threads can exist.
                let _ = thread::Builder::new()
                    .name("gwnative-http".into())
                    .spawn(move || {
                        let _connection = connection;
                        qos::set(qos::Class::UserInitiated);
                        let _ = serve(stream, &context);
                    });
            }
        })?;

    Ok(Loopback {
        addr,
        settings,
        recorder,
    })
}

/// Take [`PORT`], or an ephemeral port if something else already holds it.
///
/// Falling back keeps the app launchable, but it is worth saying out loud: the
/// page will come up on a different origin and so will not find anything it
/// stored on previous launches.
fn bind(port: u16) -> std::io::Result<TcpListener> {
    let deadline = Instant::now() + PORT_PATIENCE;
    loop {
        match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() != std::io::ErrorKind::AddrInUse => return Err(e),
            Err(_) => {}
        }
        // Taken — and only a relaunch is willing to wait for it. It waited for
        // the instance lock already, but the lock is let go first: a process
        // exits by closing its descriptors in order and the lock was opened
        // before this socket, so a successor can get this far while its
        // predecessor's listener is still a moment from closing. The gap is
        // microseconds wide and it is the difference between a relaunch that
        // keeps everything the page stored and one that quietly loses it.
        if !relaunch::is_successor() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(PORT_POLL);
    }

    note!(
        "[loopback] port {port} is taken, falling back to an ephemeral one; \
         saved page state will not be found this session"
    );
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

/// What to do with the connection once a request has been answered.
enum Flow {
    /// Wait for another request on the same connection.
    Keep,
    Close,
    /// The page upgraded this connection to a socket bridge. It has stopped
    /// being HTTP, so [`serve`] hands both halves to `sockets` and stops.
    Bridge(String, Option<generation::LaunchIdentity>),
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
            Flow::Bridge(destination, gameplay_launch) => {
                // A game socket is idle for long stretches by design, so the
                // HTTP idle timeout must not follow it across the upgrade.
                stream.set_read_timeout(None)?;
                stream.set_write_timeout(None)?;
                // The reader goes too: anything the peer sent after the request
                // head is already inside its buffer, and a fresh reader over the
                // same socket would drop those bytes.
                sockets::bridge(
                    reader,
                    stream,
                    &destination,
                    gameplay_launch,
                    &context.sockets,
                );
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
        None => content::serve(&request, stream, context),
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

    fn capability_tokens(browser: &str) -> CapabilityTokens {
        CapabilityTokens {
            browser: browser.to_owned(),
            game_reader: "game-reader-token".to_owned(),
            game_publisher: "game-publisher-token".to_owned(),
        }
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
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: dir.clone(),
            snapshot: None,
            recorder: Recorder::open(dir.join("diagnostics")),
            derived_wasm: wasm::DerivedModules::default(),
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                file.clone(),
            ))),
            generations: Arc::new(generation::Store::open(dir.join("generations"))),
            tokens: capability_tokens(token),
            port: PORT,
            credential_account: "login".into(),
        })
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

    #[test]
    fn game_state_read_and_publish_capabilities_are_disjoint() {
        let temp = TempDir::new("server-game-api");
        let dir = temp.0.clone();
        let browser = "browser-token";
        let reader = "game-reader-token";
        let publisher = "game-publisher-token";
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: dir.clone(),
            snapshot: None,
            recorder: Recorder::open(dir.join("diagnostics")),
            derived_wasm: wasm::DerivedModules::default(),
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                dir.join("settings.json"),
            ))),
            generations: Arc::new(generation::Store::open(dir.join("generations"))),
            tokens: capability_tokens(browser),
            port: PORT,
            credential_account: "login".into(),
        })
        .unwrap();
        let address = loopback.addr;

        assert_eq!(request(address, "GET", "/__game/v1", None, "").0, 403);
        let (status, description) = request(address, "GET", "/__game/v1", Some(reader), "");
        assert_eq!(status, 200);
        assert!(description.contains(r#""longPoll":true"#), "{description}");
        assert!(
            description.contains(r#""available":false"#),
            "{description}"
        );

        assert_eq!(
            request(address, "GET", "/__credentials", Some(reader), "").0,
            403,
        );
        assert_eq!(
            request(
                address,
                "PUT",
                "/__game/v1/state",
                Some(reader),
                r#"{"status":"waiting"}"#,
            )
            .0,
            403,
        );
        assert_eq!(
            request(
                address,
                "PUT",
                "/__game/v1/state",
                Some(browser),
                r#"{"status":"waiting"}"#,
            )
            .0,
            403,
        );
        assert_eq!(
            request(address, "GET", "/__game/v1/state", Some(publisher), "").0,
            403,
        );

        let ready = r#"{"status":"ready","mapId":55,"playerId":2,"playerX":0,"playerY":0,"targetValid":false}"#;
        assert_eq!(
            request(address, "PUT", "/__game/v1/state", Some(publisher), ready,).0,
            200,
        );
        let (status, state) = request(address, "GET", "/__game/v1/state", Some(reader), "");
        assert_eq!(status, 200);
        assert!(state.contains(r#""revision":1"#), "{state}");
        assert!(state.contains(r#""mapId":55"#), "{state}");
        assert_eq!(
            request(
                address,
                "GET",
                "/__game/v1/state?after=1&waitMs=0",
                Some(reader),
                "",
            )
            .0,
            404,
        );

        assert_eq!(
            request(
                address,
                "PUT",
                "/__game/v1/state",
                Some(publisher),
                r#"{"status":"unsupported","reason":"producer stopped"}"#,
            )
            .0,
            200,
        );
        let (status, state) = request(address, "GET", "/__game/v1/state?after=1", Some(reader), "");
        assert_eq!(status, 200);
        assert!(state.contains(r#""status":"unsupported""#), "{state}");
        assert!(!state.contains("mapId"), "stale state leaked: {state}");
        let (_, description) = request(address, "GET", "/__game/v1", Some(browser), "");
        assert!(
            description.contains(r#""available":false"#),
            "{description}"
        );

        assert_eq!(
            request(address, "POST", "/__game/v1/actions", Some(reader), "{}").0,
            403,
        );
        assert_eq!(
            request(address, "POST", "/__game/v1/actions", Some(browser), "{}").0,
            409,
        );
    }

    /// The route the settings panel's "Clear Game Data…" reaches.
    ///
    /// What it does when there *is* a store is `cache::request_clear`, which has
    /// its own tests and does not need a socket to be shown. What only the wire
    /// can show is the rest: that the route exists at all, that the same gate
    /// covers it, and that a launch with no snapshot answers rather than
    /// arming a clear for a directory this process never opened.
    #[test]
    fn the_clear_route_is_gated_and_refuses_what_it_cannot_clear() {
        let temp = TempDir::new("server-clear");
        let dir = temp.0.clone();
        let token = "test-token";
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: dir.clone(),
            // The interesting half of this test: no store, which is every launch
            // before the manifest is fetched and every launch that failed to get
            // one.
            snapshot: None,
            recorder: Recorder::open(dir.join("diagnostics")),
            derived_wasm: wasm::DerivedModules::default(),
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                dir.join("settings.json"),
            ))),
            generations: Arc::new(generation::Store::open(dir.join("generations"))),
            tokens: capability_tokens(token),
            port: PORT,
            credential_account: "login".into(),
        })
        .unwrap();
        let addr = loopback.addr;

        assert_eq!(
            request(addr, "DELETE", "/__data", None, "").0,
            403,
            "the gate comes before anything the route decides",
        );
        // Handled by falling through, which is what every `__` route does when
        // the thing it speaks for is not there — the page treats it as "nothing
        // to report on" and takes the section away.
        assert_eq!(request(addr, "DELETE", "/__data", Some(token), "").0, 404);
        // Same answer, for the same reason, from the route the panel reads the
        // figure from. Both used to fall through to the static file server and
        // come back 403, which reads as "you may not ask" rather than "there is
        // nothing to ask about".
        assert_eq!(request(addr, "GET", "/__prefetch", Some(token), "").0, 404);

        // And no marker was left behind for the next launch to act on.
        assert!(!dir.join("chunks.clear").exists());
    }

    /// What the page prints has to reach the file the player can send on.
    ///
    /// Worth a wire test because the two halves were written years apart and
    /// neither notices the other: `harness.js` wraps `console.*` and posts a
    /// batch here, and for a long time this route only echoed it to stderr —
    /// which is nobody's, on a build a player is running. The batch arriving
    /// and the batch being recorded are separate facts, and only this test
    /// holds them together.
    #[test]
    fn a_batch_the_page_posted_lands_in_the_file_the_player_can_send() {
        let temp = TempDir::new("server-report");
        let dir = temp.0.clone();
        let token = "test-token";
        let diagnostics = dir.join("diagnostics");
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: dir.clone(),
            snapshot: None,
            recorder: Recorder::open(diagnostics.clone()),
            derived_wasm: wasm::DerivedModules::default(),
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                dir.join("settings.json"),
            ))),
            generations: Arc::new(generation::Store::open(dir.join("generations"))),
            tokens: capability_tokens(token),
            port: PORT,
            credential_account: "login".into(),
        })
        .unwrap();
        let addr = loopback.addr;

        assert_eq!(
            request(addr, "POST", "/__report", None, "[warn] leaked\n").0,
            403,
            "an untokened page must not be able to write into the report",
        );
        assert_eq!(
            request(
                addr,
                "POST",
                "/__report",
                Some(token),
                "[warn] first\nsecond\n"
            )
            .0,
            204,
        );

        // The recorder writes on the calling thread, so by the time the answer
        // is back the lines are on disk — no polling needed.
        let body = std::fs::read_to_string(diagnostics.join("gwnative.jsonl")).unwrap();
        let lines: Vec<&str> = body
            .lines()
            .filter(|line| line.contains("\"kind\":\"page\""))
            .collect();
        assert_eq!(lines.len(), 2, "one record per printed line, in {body}");
        assert!(lines[0].contains("[warn] first"));
        assert!(lines[1].contains("second"));
        assert!(
            !body.contains("leaked"),
            "the refused batch was recorded anyway: {body}",
        );
    }

    #[test]
    fn a_failed_transform_can_request_the_exact_official_module() {
        let temp = TempDir::new("server-original-wasm");
        let dir = temp.0.clone();
        let shell = dir.join("shell");
        std::fs::create_dir(&shell).unwrap();
        std::fs::write(dir.join("index.html"), b"stale shell").unwrap();
        std::fs::write(shell.join("index.html"), b"reviewed shell").unwrap();
        std::fs::write(shell.join("Gw.wasm"), b"counterfeit").unwrap();
        std::fs::write(dir.join("Gw.wasm"), b"official").unwrap();
        let transformed = dir.join("transformed.wasm");
        std::fs::write(&transformed, b"transformed").unwrap();
        let mut derived = wasm::DerivedModules::default();
        derived.insert(wasm::Runtime::Asyncify, transformed);
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: shell,
            snapshot: None,
            recorder: Recorder::open(dir.join("diagnostics")),
            derived_wasm: derived,
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                dir.join("settings.json"),
            ))),
            generations: Arc::new(generation::Store::open(dir.join("generations"))),
            tokens: capability_tokens("test-token"),
            port: PORT,
            credential_account: "login".into(),
        })
        .unwrap();

        assert_eq!(
            request(loopback.addr, "GET", "/index.html", None, ""),
            (200, "reviewed shell".to_owned())
        );
        assert_eq!(
            request(loopback.addr, "GET", "/Gw.wasm", None, ""),
            (200, "transformed".to_owned())
        );
        assert_eq!(
            request(
                loopback.addr,
                "GET",
                "/Gw.wasm?gwnative-original=1",
                None,
                ""
            ),
            (200, "official".to_owned())
        );
    }

    #[test]
    fn runtime_fallback_state_is_token_gated_and_strictly_validated() {
        const BUILD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let temp = TempDir::new("server-runtime-state");
        let dir = temp.0.clone();
        let token = "test-token";
        let generations = Arc::new(generation::Store::open(dir.join("generations")));
        std::fs::write(dir.join("Gw.jspi.js"), b"glue").unwrap();
        std::fs::write(dir.join("Gw.jspi.wasm"), b"wasm").unwrap();
        std::fs::write(dir.join("manifest.cache"), b"manifest").unwrap();
        generations.record("test", &dir, &["Gw.jspi.js", "Gw.jspi.wasm"]);
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: dir.clone(),
            snapshot: None,
            recorder: Recorder::open(dir.join("diagnostics")),
            derived_wasm: wasm::DerivedModules::default(),
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                dir.join("settings.json"),
            ))),
            generations: Arc::clone(&generations),
            tokens: capability_tokens(token),
            port: PORT,
            credential_account: "login".into(),
        })
        .unwrap();

        let attempt = format!(
            r#"{{"runtime":"jspi","build":"{BUILD}","transformed":true,"nonce":"{BUILD}"}}"#
        );
        assert_eq!(
            request(loopback.addr, "POST", "/__runtime", None, &attempt).0,
            403
        );
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__runtime",
                Some(token),
                &format!(
                    r#"{{"runtime":"other","build":null,"transformed":false,"nonce":"{BUILD}"}}"#
                )
            )
            .0,
            400
        );
        let (status, identity) =
            request(loopback.addr, "POST", "/__runtime", Some(token), &attempt);
        assert_eq!(status, 200);
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__transform-failed",
                Some(token),
                &format!(r#"{{"launch":{identity}}}"#)
            )
            .0,
            204
        );
        assert!(generations.transform_disabled("jspi", BUILD));

        let original = format!(
            r#"{{"runtime":"jspi","build":"{BUILD}","transformed":false,"nonce":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}}"#
        );
        let (status, launch) = request(loopback.addr, "POST", "/__runtime", Some(token), &original);
        assert_eq!(status, 200);
        let proof = format!(r#"{{"launch":{launch}}}"#);
        assert_eq!(
            request(loopback.addr, "POST", "/__booted", None, &proof).0,
            403
        );
        let mut stale: serde_json::Value = serde_json::from_str(&launch).unwrap();
        stale["nonce"] = BUILD.into();
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__booted",
                Some(token),
                &format!(r#"{{"launch":{stale}}}"#),
            )
            .0,
            400
        );
        assert_eq!(
            request(loopback.addr, "POST", "/__booted", Some(token), &proof).0,
            204
        );
        assert_eq!(
            request(loopback.addr, "POST", "/__booted", Some(token), &proof).0,
            204,
            "a lost acknowledgement must be safe to retry"
        );
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__booted",
                Some(token),
                &format!(r#"{{"launch":{stale}}}"#),
            )
            .0,
            400,
            "different data is not an idempotent duplicate"
        );

        // A valid request is a server failure, not an acknowledgement, when the
        // journal cannot make the attempt durable. The page must not execute
        // glue after this response.
        std::fs::remove_file(dir.join("generations/state.json")).unwrap();
        std::fs::create_dir(dir.join("generations/state.json")).unwrap();
        assert_eq!(
            request(loopback.addr, "POST", "/__runtime", Some(token), &attempt).0,
            500
        );
    }

    #[test]
    fn cross_runtime_plan_is_exact_persisted_and_token_gated() {
        const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const NEXT_NONCE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let temp = TempDir::new("server-runtime-plan");
        let dir = temp.0.clone();
        let token = "test-token";
        let generations = Arc::new(generation::Store::open(dir.join("generations")));
        let names = [
            "Gw.jspi.js",
            "Gw.jspi.wasm",
            "Gw.js",
            "Gw.wasm",
            "version.json",
        ];
        for name in names {
            std::fs::write(dir.join(name), name).unwrap();
        }
        std::fs::write(dir.join("manifest.cache"), b"manifest").unwrap();
        assert!(generations.record("test", &dir, &names));
        let loopback = spawn(Config {
            root: dir.clone(),
            shell_root: dir.clone(),
            snapshot: None,
            recorder: Recorder::open(dir.join("diagnostics")),
            derived_wasm: wasm::DerivedModules::default(),
            settings: Arc::new(settings::ScopedStore::single(settings::Store::open(
                dir.join("settings.json"),
            ))),
            generations: Arc::clone(&generations),
            tokens: capability_tokens(token),
            port: PORT,
            credential_account: "login".into(),
        })
        .unwrap();

        assert_eq!(
            request(loopback.addr, "GET", "/__runtime-plan", None, "").0,
            403
        );
        assert_eq!(
            request(loopback.addr, "GET", "/__runtime-plan", Some(token), ""),
            (200, r#"{"failedOfficial":[]}"#.to_owned())
        );

        let attempt = |runtime: &str, nonce: &str| {
            request(
                loopback.addr,
                "POST",
                "/__runtime",
                Some(token),
                &format!(
                    r#"{{"runtime":"{runtime}","build":null,"transformed":false,"nonce":"{nonce}"}}"#
                ),
            )
        };
        let (status, jspi) = attempt("jspi", NONCE);
        assert_eq!(status, 200);
        let mut stale: serde_json::Value = serde_json::from_str(&jspi).unwrap();
        stale["nonce"] = NEXT_NONCE.into();
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__runtime-failed",
                Some(token),
                &format!(r#"{{"launch":{stale}}}"#),
            )
            .0,
            400
        );
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__runtime-failed",
                Some(token),
                &format!(r#"{{"launch":{jspi}}}"#),
            ),
            (
                200,
                r#"{"outcome":"try-runtime","runtime":"asyncify"}"#.to_owned()
            )
        );
        assert_eq!(
            request(loopback.addr, "GET", "/__runtime-plan", Some(token), ""),
            (200, r#"{"failedOfficial":["jspi"]}"#.to_owned())
        );

        let (status, asyncify) = attempt("asyncify", NEXT_NONCE);
        assert_eq!(status, 200);
        assert_eq!(
            request(
                loopback.addr,
                "POST",
                "/__runtime-failed",
                Some(token),
                &format!(r#"{{"launch":{asyncify}}}"#),
            ),
            (200, r#"{"outcome":"exhausted"}"#.to_owned())
        );
        assert!(!generations.rejected("test"));
    }
}
