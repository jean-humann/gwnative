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
//! `Gw.snapshot` is served from here too, as a virtual ranged file. It is 4.2 GB
//! and the client reads a small fraction of it per session, so the bytes come
//! from the chunk store on demand rather than from disk.

use std::io::{BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crate::app;
use crate::chunks::ChunkStore;
use crate::diagnostics::{self, Recorder};
use crate::disk;
use crate::generation;
use crate::http::{
    BODY_BUFFER, MAX_BODY_BYTES, POLICY, Request, common_headers, etag, mime, parse_range, policy,
    read_request, resolve, respond, respond_head, respond_not_modified, respond_proxy,
    respond_static, token_matches,
};
use crate::patch::SNAPSHOT;
use crate::settings;
use crate::sockets::{self, Registry};
use crate::{keychain, net, proxy, qos, ws};

/// Largest span served from one request. The harness asks for far less; this
/// only stops a stray `Range: bytes=0-` from walking the whole image in one go.
///
/// 8 MiB, the figure gwonmac settled on, not the 32 MiB this used to be. The
/// body is streamed now so the cap no longer bounds memory, but it still bounds
/// how long one request occupies a connection and a fetch slot: 32 MiB is 128
/// chunks, and a demand read arriving behind that waits for all of them.
const MAX_RANGE_BYTES: u64 = 8 * 1024 * 1024;

/// Room to leave behind after a full download.
///
/// A volume with nothing left is not merely full: swap, the WebKit caches and
/// every atomic rename in this process all want space, and the first thing to
/// break would be the game rather than the download that caused it. 2 GB is
/// generous next to a 4.2 GB snapshot and cheap next to the alternative.
const DISK_HEADROOM: u64 = 2 * 1024 * 1024 * 1024;

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
            eprintln!(
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

fn handle(
    reader: &mut BufReader<TcpStream>,
    stream: &mut TcpStream,
    context: &Context,
) -> std::io::Result<Flow> {
    let Some(request) = read_request(reader)? else {
        return Ok(Flow::Close);
    };

    if request.content_length > MAX_BODY_BYTES {
        respond(
            stream,
            413,
            "Payload Too Large",
            "text/plain",
            b"too large",
            &[],
        )?;
        return Ok(Flow::Close);
    }

    // Whether this connection survives is decided once, here, so that every
    // `respond` below can stay a tail call that says nothing about framing.
    let flow = if request.close {
        Flow::Close
    } else {
        Flow::Keep
    };

    if tracing() {
        eprintln!("[loopback] -> {} /{}", request.method, request.path);
    }

    // Every `__` route is a host capability rather than page content: name
    // resolution, an outbound TCP bridge, the keychain, the download control,
    // the residency map. Loopback is host-wide — any process running as this
    // user can reach this port — so the token is the whole of what separates
    // the page from everything else on the machine, and it belongs on all of
    // them rather than only on the one that happens to touch a password.
    // `Gw.snapshot` and the static files stay open, because the vendored client
    // requests those itself and cannot be taught to carry a token.
    if request.path.starts_with("__")
        && !token_matches(&context.token, request.offered_token().as_deref())
    {
        eprintln!(
            "[loopback] refused an untokened {} /{}",
            request.method, request.path
        );
        respond(stream, 403, "Forbidden", "text/plain", b"forbidden", &[])?;
        return Ok(flow);
    }

    // Diagnostic channel for the bring-up harness. Real host calls will go over
    // WKScriptMessageHandlerWithReply; this only exists so a page can report
    // results without a UI.
    if request.method == "POST" && request.path == "__report" {
        eprintln!("[report] {}", String::from_utf8_lossy(&request.body));
        respond(stream, 204, "No Content", "text/plain", b"", &[])?;
        return Ok(flow);
    }

    // The game asks for an address before it dials. Answering here keeps name
    // resolution on the host, where the public-unicast policy lives.
    if request.path == "__dns" {
        let name = request.param("name").unwrap_or_default();
        match net::resolve(&name) {
            Ok(address) => {
                if tracing() {
                    eprintln!("[dns] {name} -> {address}");
                }
                respond(
                    stream,
                    200,
                    "OK",
                    "text/plain",
                    address.to_string().as_bytes(),
                    &[],
                )?;
            }
            Err(e) => {
                eprintln!("[dns] {name}: {e}");
                respond(
                    stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    e.to_string().as_bytes(),
                    &[],
                )?;
            }
        }
        return Ok(flow);
    }

    // Saved login. Already gated above with every other `__` route.
    if request.path == "__credentials" {
        match request.method.as_str() {
            "GET" => match keychain::load() {
                Some(credentials) => {
                    let body = serde_json::to_vec(&credentials).unwrap_or_default();
                    eprintln!("[credentials] read from the keychain");
                    respond(stream, 200, "OK", "application/json", &body, &[])?;
                }
                // Not an error: a first launch has nothing saved, and the client
                // treats "none" as "ask the player".
                None => {
                    eprintln!("[credentials] nothing saved yet");
                    respond(
                        stream,
                        404,
                        "Not Found",
                        "text/plain",
                        b"no stored credentials",
                        &[],
                    )?;
                }
            },
            "PUT" => {
                let stored = serde_json::from_slice(&request.body)
                    .map_err(|e| e.to_string())
                    .and_then(|c: keychain::Credentials| keychain::store(&c));
                match stored {
                    Ok(()) => {
                        eprintln!("[credentials] saved to the keychain");
                        respond(stream, 204, "No Content", "text/plain", b"", &[])?;
                    }
                    Err(e) => {
                        eprintln!("[credentials] not saved: {e}");
                        respond(stream, 400, "Bad Request", "text/plain", e.as_bytes(), &[])?;
                    }
                }
            }
            "DELETE" => {
                keychain::clear();
                eprintln!("[credentials] cleared");
                respond(stream, 204, "No Content", "text/plain", b"", &[])?;
            }
            _ => respond(
                stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"use GET, PUT or DELETE",
                &[("Allow", "GET, PUT, DELETE".into())],
            )?,
        }
        return Ok(flow);
    }

    // What the player chose. GET is the authoritative read — the page is handed
    // a copy at document start, but a settings window opened an hour later must
    // not show what was true at launch. PUT takes a patch and answers with the
    // whole, so the page never has to guess what its change merged into.
    if request.path == "__settings" {
        match request.method.as_str() {
            "GET" => {
                let body = serde_json::to_vec(&context.settings.get()).unwrap_or_default();
                respond(stream, 200, "OK", "application/json", &body, &[])?;
            }
            "PUT" => {
                let applied = serde_json::from_slice(&request.body)
                    .map_err(|e| e.to_string())
                    .and_then(|raw| context.settings.apply(&raw));
                match applied {
                    Ok(settings) => {
                        let body = serde_json::to_vec(&settings).unwrap_or_default();
                        respond(stream, 200, "OK", "application/json", &body, &[])?;
                    }
                    // A refused patch is a bug in the page, not a player error,
                    // so it is said out loud rather than only answered with 400.
                    Err(e) => {
                        eprintln!("[settings] refused a patch: {e}");
                        respond(stream, 400, "Bad Request", "text/plain", e.as_bytes(), &[])?;
                    }
                }
            }
            _ => respond(
                stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"use GET or PUT",
                &[("Allow", "GET, PUT".into())],
            )?,
        }
        return Ok(flow);
    }

    if request.path == "__socket" {
        if !request.wants_websocket() {
            respond(
                stream,
                426,
                "Upgrade Required",
                "text/plain",
                b"__socket is a websocket endpoint",
                &[("Upgrade", "websocket".into())],
            )?;
            return Ok(flow);
        }
        let destination = request.param("to").unwrap_or_default();
        ws::accept(stream, request.websocket_key.as_deref().unwrap_or(""))?;
        return Ok(Flow::Bridge(destination));
    }

    if request.path == "__resident"
        && let Some(store) = &context.snapshot
    {
        let bits = store.resident_bitmap();
        respond(
            stream,
            200,
            "OK",
            "application/octet-stream",
            &bits,
            &[("X-Chunk-Size", store.chunk_size().to_string())],
        )?;
        return Ok(flow);
    }

    // The page's own metrics. POST folds a batch in, GET reads back everything
    // the host has — its own sampler's figures included, so one fetch answers
    // "what is this process doing" from inside the page.
    if request.path == "__diag" {
        if request.method == "POST" {
            match serde_json::from_slice(&request.body) {
                Ok(body) => diagnostics::absorb(&context.recorder.metrics, &body),
                Err(e) => eprintln!("[diag] ignoring a malformed batch: {e}"),
            }
            // Nothing back. The page posts a batch every second and never reads
            // the reply, so answering with the full snapshot meant serializing
            // every metric the host holds once a second and handing the page a
            // response body it would never drain — one more each second, all of
            // them growing, for the life of the session.
            respond(stream, 204, "No Content", "text/plain", b"", &[])?;
            return Ok(flow);
        }
        let usage = diagnostics::usage().unwrap_or_default();
        // The chunk store's own tally rides along rather than living on a route
        // of its own. It used to be `__stats`, which nothing ever called: a
        // second endpoint to remember, answering a question this one is already
        // the place to ask.
        let (from_cache, fetched, coalesced) = context
            .snapshot
            .as_ref()
            .map_or((0, 0, 0), |store| store.stats());
        let body = serde_json::json!({
            "footprintMiB": usage.footprint as f64 / 1048576.0,
            "cpuSeconds": usage.cpu().as_secs_f64(),
            "chunks": { "fromCache": from_cache, "fetched": fetched, "coalesced": coalesced },
            "metrics": context.recorder.metrics.snapshot(),
        });
        respond(
            stream,
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            &[],
        )?;
        return Ok(flow);
    }

    // The harness says the first frame is up. Two things follow from that.
    // Everything the store read before now is what booting costs, so that is
    // the list worth warming next time — and the client on disk has just proved
    // it runs here, which is the only evidence a freshly synced build can offer
    // and the thing the generation record is waiting for. Deliberately one
    // route: a second one would be a second chance to disagree about what
    // "it booted" means.
    if request.path == "__booted" {
        if let Some(store) = &context.snapshot {
            store.seal_boot_list();
        }
        context.generations.prove();
        respond(stream, 204, "No Content", "text/plain", b"", &[])?;
        return Ok(flow);
    }

    // The client has exited cleanly and there is nothing left on screen but its
    // last frame. Answered before quitting rather than after, because
    // `terminate:` runs the flush and the reply would otherwise race it.
    if request.method == "POST" && request.path == "__quit" {
        respond(stream, 204, "No Content", "text/plain", b"", &[])?;
        app::request_quit();
        return Ok(Flow::Close);
    }

    // Full download: POST starts or stops the background sweep, GET polls it.
    // The launcher offers this as the alternative to streaming on demand.
    if request.path == "__prefetch"
        && let Some(store) = &context.snapshot
    {
        // What a full download would still have to write, and what the volume
        // says it could take. Both are reported on every poll so the page can
        // show the price before the player agrees to it — asking for 4.2 GB and
        // then filling the disk is the failure this exists to avoid.
        let outstanding =
            (store.chunk_count() - store.resident_count()) as u64 * store.chunk_size();
        let free = disk::available(store.cache_dir());

        if request.method == "POST" {
            if request.query == "stop" {
                store.stop_full_download();
            } else if let Some(free) = free.filter(|free| *free < outstanding + DISK_HEADROOM) {
                // Refused rather than started-and-abandoned: a sweep that runs
                // until the volume fills takes the rest of the machine down
                // with it, and the client streams perfectly well without it.
                eprintln!(
                    "[prefetch] refused: {:.1} GB free, {:.1} GB needed",
                    free as f64 / 1e9,
                    (outstanding + DISK_HEADROOM) as f64 / 1e9
                );
                let body = format!(
                    r#"{{"error":"not enough room","free":{free},"needed":{}}}"#,
                    outstanding + DISK_HEADROOM
                );
                respond(
                    stream,
                    507,
                    "Insufficient Storage",
                    "application/json",
                    body.as_bytes(),
                    &[],
                )?;
                return Ok(flow);
            } else {
                store.start_full_download();
            }
        }
        // `cached` is residency, not sweep progress: the launcher's question is
        // how much of the game is already paid for, and a sweep's own counter
        // restarts at zero every time one does. `fetched` is kept beside it
        // because it is the only number that moves when a sweep is re-walking
        // ground it already has, which is what "running but not advancing"
        // looks like from the page.
        let (fetched, _, running) = store.prefetch_progress();
        let cached = store.resident_count();
        let total = store.chunk_count();
        let chunk_size = store.chunk_size();
        // `null` rather than a guess when the volume will not say: the page
        // treats not knowing as no reason to stop, which is the same thing it
        // does when this whole route is missing.
        let free = free.map_or("null".to_owned(), |free| free.to_string());
        // Two numbers, because they answer different questions: `outstanding`
        // is what the download would write and belongs in the prose, `needed`
        // is what the volume must have and is what the refusal above compares.
        let needed = outstanding + DISK_HEADROOM;
        let body = format!(
            r#"{{"cached":{cached},"total":{total},"fetched":{fetched},"running":{running},"chunkSize":{chunk_size},"outstanding":{outstanding},"needed":{needed},"free":{free}}}"#
        );
        respond(stream, 200, "OK", "application/json", body.as_bytes(), &[])?;
        return Ok(flow);
    }

    if request.path == SNAPSHOT
        && let Some(store) = &context.snapshot
    {
        // Worth timing rather than merely counting: a range that has to fetch
        // the chunks it covers blocks the client for as long as it takes, and
        // it is the one host-side latency the player feels directly. The mean
        // hides that, so keep the worst as well.
        let began = std::time::Instant::now();
        // A range that fails part way through has already put bytes on the wire
        // under a Content-Length it can no longer honour. There is no way back
        // to a clean message boundary, so the connection ends here.
        let intact = serve_snapshot(stream, &request, store)?;
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        let metrics = &context.recorder.metrics;
        metrics.count("gw.range.requests", 1.0);
        metrics.gauge("gw.range.ms", ms);
        metrics.peak("gw.range.ms.max", ms);
        return Ok(match intact {
            true => flow,
            false => Flow::Close,
        });
    }

    // The client's own web requests, which it addressed to this origin because
    // its glue rewrote them. See `proxy` for why, and why the table is closed.
    let (route, tail) = match request.path.split_once('/') {
        Some((route, tail)) => (route, format!("/{tail}")),
        None => (request.path.as_str(), "/".to_owned()),
    };
    if proxy::host(route).is_some() {
        if request.body.len() > proxy::MAX_BODY {
            respond(
                stream,
                413,
                "Payload Too Large",
                "text/plain",
                b"request body too large",
                &[],
            )?;
            return Ok(flow);
        }
        match proxy::forward(
            route,
            &tail,
            &request.query,
            &request.method,
            &request.headers,
            request.body,
        ) {
            Ok(reply) => {
                eprintln!(
                    "[proxy] {} /{route}{tail} -> {}",
                    request.method, reply.status
                );
                respond_proxy(stream, &reply)?;
                // `respond_proxy` relays upstream's headers, and upstream's
                // framing is not ours to reinterpret, so this hop ends here.
                return Ok(Flow::Close);
            }
            Err(e) => {
                eprintln!("[proxy] {} /{route}{tail}: {e}", request.method);
                respond(
                    stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    b"proxy error",
                    &[],
                )?;
                return Ok(flow);
            }
        }
    }

    // The derived client answers to the base module's own name, so the page
    // asks for one thing and the glue's `locateFile` needs no special case.
    let derived = context
        .derived_wasm
        .as_ref()
        .filter(|_| request.path == "Gw.jspi.wasm")
        .cloned();
    match derived.or_else(|| resolve(&context.root, &request.path)) {
        Some(file) => {
            let tag = std::fs::metadata(&file).ok().as_ref().and_then(etag);
            // Answer the validator before reading the file, not after: skipping
            // the 8.2 MB read is half of what the validator is for.
            if let Some(tag) = &tag
                && request.if_none_match.as_deref() == Some(tag.as_str())
            {
                if tracing() {
                    eprintln!("[loopback] 304 /{}", request.path);
                }
                respond_not_modified(stream, tag)?;
                return Ok(flow);
            }
            match std::fs::read(&file) {
                Ok(body) => {
                    if tracing() {
                        eprintln!("[loopback] 200 /{} ({} bytes)", request.path, body.len());
                    }
                    respond_static(stream, mime(&file), &body, tag.as_deref())?;
                }
                Err(_) => {
                    eprintln!("[loopback] 404 /{}", request.path);
                    respond(stream, 404, "Not Found", "text/plain", b"not found", &[])?;
                }
            }
        }
        None => {
            eprintln!("[loopback] 403 /{}", request.path);
            respond(stream, 403, "Forbidden", "text/plain", b"forbidden", &[])?;
        }
    }
    Ok(flow)
}

/// Answer a snapshot range, streaming the body rather than assembling it.
///
/// Returns whether the connection is still usable: `false` means the head was
/// already on the wire when the body failed, so the caller must close.
fn serve_snapshot(
    stream: &mut TcpStream,
    request: &Request,
    store: &ChunkStore,
) -> std::io::Result<bool> {
    let total = store.snapshot_size();

    // A HEAD is how the client learns the image size without pulling a byte.
    if request.method == "HEAD" {
        respond_head(stream, total, &[("Accept-Ranges", "bytes".into())])?;
        return Ok(true);
    }

    let Some((start, end)) = request.range.as_deref().and_then(|r| parse_range(r, total)) else {
        // Refusing an unranged GET is deliberate: answering it means streaming
        // 4.2 GB, which is never what the client wants.
        respond(
            stream,
            416,
            "Range Not Satisfiable",
            "text/plain",
            b"snapshot requires a byte range",
            &[("Content-Range", format!("bytes */{total}"))],
        )?;
        return Ok(true);
    };

    let length = (end - start + 1).min(MAX_RANGE_BYTES);
    // Arithmetic, not I/O — which is the point. The old shape read the whole
    // span into a Vec purely to learn how long it was, so a 32 MiB range meant
    // 32 MiB resident in the host before a single byte reached the socket.
    let produced = store.readable(start, length);
    if produced == 0 {
        respond(
            stream,
            416,
            "Range Not Satisfiable",
            "text/plain",
            b"range starts past the end of the snapshot",
            &[("Content-Range", format!("bytes */{total}"))],
        )?;
        return Ok(true);
    }

    // Chunks arrive as they are read, so the first one can be on the wire while
    // the last is still being fetched — and the peak allocation is one chunk
    // rather than the whole range.
    let last = start + produced - 1;
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\n{}",
        common_headers(
            "application/octet-stream",
            produced,
            "no-store",
            &[
                ("Content-Range", format!("bytes {start}-{last}/{total}")),
                ("Accept-Ranges", "bytes".into()),
            ],
        )
    );
    stream.write_all(head.as_bytes())?;

    let mut body = std::io::BufWriter::with_capacity(BODY_BUFFER, &mut *stream);
    if let Err(e) = store.read_into(start, length, &mut body) {
        eprintln!("[loopback] /{SNAPSHOT} {start}+{length} failed mid-body: {e}");
        return Ok(false);
    }
    body.flush()?;
    drop(body);
    stream.flush()?;

    if tracing() {
        eprintln!("[loopback] 206 /{SNAPSHOT} bytes {start}-{last}/{total} ({produced} bytes)");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that is this test's alone, named for it.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gwnative-server-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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
        let dir = scratch("settings");
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
    }
}
