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

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;

use crate::chunks::ChunkStore;
use crate::patch::SNAPSHOT;
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

/// The origin's port. Any constant would do; this one is unassigned by IANA and
/// sits below macOS's ephemeral floor of 49152, so the kernel will not hand it
/// to somebody else's outbound socket while we are not listening.
///
/// `GWNATIVE_PORT` overrides it, which is also how a second instance gets its
/// own private store rather than fighting over this one.
const PORT: u16 = 38112;

pub struct Loopback {
    pub addr: SocketAddr,
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
    token: String,
) -> std::io::Result<Loopback> {
    let listener = bind()?;
    let addr = listener.local_addr()?;
    let context = Arc::new(Context {
        root,
        snapshot,
        sockets: Arc::default(),
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

    Ok(Loopback { addr })
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

struct Request {
    method: String,
    path: String,
    query: String,
    range: Option<String>,
    content_length: usize,
    upgrade: Option<String>,
    websocket_key: Option<String>,
    token: Option<String>,
    /// The validator the page already holds for a static file, if any.
    if_none_match: Option<String>,
    /// Every header, lower-cased, in arrival order. Only the proxy needs them:
    /// it has to forward what the page sent rather than a reconstruction of it.
    headers: Vec<(String, String)>,
    /// Read up front rather than by whichever handler wants it. On a kept-alive
    /// connection a body left unread is not merely ignored — it sits in the
    /// stream and gets parsed as the next request line.
    body: Vec<u8>,
    /// The peer asked to close after this exchange.
    close: bool,
}

impl Request {
    /// First value for `name` in the query string, percent-decoded.
    fn param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| percent_decode(value))
        })
    }

    /// The token the caller offered, by header or by query string.
    ///
    /// The header is the right place for it and is what every `fetch` here
    /// uses. The query string exists for one caller: a browser `WebSocket`
    /// cannot set request headers, so `__socket` — the route that bridges to
    /// arbitrary outbound TCP, and therefore the one most worth gating — has
    /// nowhere else to carry it. Nothing logs the query string.
    fn offered_token(&self) -> Option<String> {
        self.token.clone().or_else(|| self.param("token"))
    }

    fn wants_websocket(&self) -> bool {
        self.upgrade
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            && self.websocket_key.is_some()
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&value[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // A stray '%' is data, not a broken escape.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Largest body accepted on this server. Everything posted here is a log line,
/// a credential blob or a proxied API call; anything larger is a mistake, and on
/// a kept-alive connection an oversized body cannot simply be truncated.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Read one request, or `None` if the peer closed the connection first — which
/// on a kept-alive connection is the ordinary way an exchange ends, not a fault.
fn read_request(reader: &mut impl BufRead) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }

    let mut close = false;
    let mut range = None;
    let mut content_length = 0usize;
    let mut upgrade = None;
    let mut websocket_key = None;
    let mut token = None;
    let mut if_none_match = None;
    let mut headers = Vec::new();
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_owned();
        match name.as_str() {
            "range" => range = Some(value.clone()),
            "content-length" => content_length = value.parse().unwrap_or(0),
            "upgrade" => upgrade = Some(value.clone()),
            "sec-websocket-key" => websocket_key = Some(value.clone()),
            "x-gwnative-token" => token = Some(value.clone()),
            "if-none-match" => if_none_match = Some(value.clone()),
            "connection" => close = value.eq_ignore_ascii_case("close"),
            _ => {}
        }
        headers.push((name, value));
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((path, rest)) => (path, rest.split('#').next().unwrap_or("")),
        None => (target.split('#').next().unwrap_or("/"), ""),
    };

    // A body larger than the cap cannot be drained safely — reading it is the
    // denial of service it would be refusing — so the caller closes instead.
    let mut body = Vec::new();
    if content_length > 0 && content_length <= MAX_BODY_BYTES {
        body.resize(content_length, 0);
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        path: path.trim_start_matches('/').to_owned(),
        query: query.to_owned(),
        range,
        content_length,
        upgrade,
        websocket_key,
        token,
        if_none_match,
        headers,
        body,
        close,
    }))
}

/// Compare in time that does not depend on how many bytes matched, so a caller
/// cannot recover the token one byte at a time by measuring the reply.
fn token_matches(expected: &str, offered: Option<&str>) -> bool {
    let Some(offered) = offered else { return false };
    if offered.len() != expected.len() {
        return false;
    }
    expected
        .bytes()
        .zip(offered.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
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

    if request.path == "__stats"
        && let Some(store) = &context.snapshot
    {
        let (cache, net, coalesced) = store.stats();
        let body = format!(r#"{{"fromCache":{cache},"fetched":{net},"coalesced":{coalesced}}}"#);
        respond(stream, 200, "OK", "application/json", body.as_bytes(), &[])?;
        return Ok(flow);
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

    // The harness says the first frame is up. Everything the store read before
    // now is what booting costs, so that is the list worth warming next time.
    if request.path == "__booted"
        && let Some(store) = &context.snapshot
    {
        store.seal_boot_list();
        respond(stream, 204, "No Content", "text/plain", b"", &[])?;
        return Ok(flow);
    }

    // Full download: POST starts or stops the background sweep, GET polls it.
    // The launcher offers this as the alternative to streaming on demand.
    if request.path == "__prefetch"
        && let Some(store) = &context.snapshot
    {
        if request.method == "POST" {
            if request.query == "stop" {
                store.stop_full_download();
            } else {
                store.start_full_download();
            }
        }
        let (done, total, running) = store.prefetch_progress();
        let body = format!(r#"{{"done":{done},"total":{total},"running":{running}}}"#);
        respond(stream, 200, "OK", "application/json", body.as_bytes(), &[])?;
        return Ok(flow);
    }

    if request.path == SNAPSHOT
        && let Some(store) = &context.snapshot
    {
        // A range that fails part way through has already put bytes on the wire
        // under a Content-Length it can no longer honour. There is no way back
        // to a clean message boundary, so the connection ends here.
        return Ok(match serve_snapshot(stream, &request, store)? {
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

    match resolve(&context.root, &request.path) {
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

/// How much of a streamed body to gather before it goes to the socket.
///
/// A range is served chunk-window by chunk-window, and the commonest of those is
/// 472 bytes; handing each one to `write` separately would be a syscall per
/// window with `TCP_NODELAY` sending each as its own segment.
const BODY_BUFFER: usize = 64 * 1024;

/// Parse a single-span `bytes=` range into an inclusive, clamped `(start, end)`.
/// Multi-range requests are not supported and fall through as unsatisfiable.
fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (start, end) = match (first.trim(), last.trim()) {
        // A suffix range asks for the final N bytes.
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            (total.checked_sub(n)?, total - 1)
        }
        (first, "") => (first.parse().ok()?, total - 1),
        (first, last) => (
            first.parse().ok()?,
            last.parse::<u64>().ok()?.min(total - 1),
        ),
    };
    (start <= end && start < total).then_some((start, end))
}

/// Reject anything that escapes `root`. `..` and absolute components are the
/// whole attack surface of a static file server, so they are refused outright
/// rather than normalised.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let rel = if path.is_empty() { "index.html" } else { path };

    let candidate = Path::new(rel);
    if candidate
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }

    let full = root.join(candidate);
    let (Ok(full), Ok(root)) = (full.canonicalize(), root.canonicalize()) else {
        return None;
    };
    full.starts_with(&root).then_some(full)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("css") => "text/css; charset=utf-8",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Headers every response carries. COOP/COEP are what make the origin
/// cross-origin isolated; CORP lets the page embed its own subresources under
/// that policy.
///
/// There is deliberately no `Connection` header: HTTP/1.1 keeps the connection
/// open unless told otherwise, and every response written through here is
/// framed by its Content-Length, so the peer can find the next one. A boot
/// issues a couple of hundred range requests, and closing after each meant a
/// fresh handshake and a fresh 2 MiB-stack thread for every one of them.
fn common_headers(
    content_type: &str,
    length: u64,
    cache: &str,
    extra: &[(&str, String)],
) -> String {
    let mut head = format!(
        "Content-Type: {content_type}\r\n\
         Content-Length: {length}\r\n\
         Cache-Control: {cache}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n"
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    head
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\n{}",
        common_headers(content_type, body.len() as u64, "no-store", extra)
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// A static file, stored by the page but revalidated before reuse.
///
/// The blanket `no-store` this replaces was costing a recompile per launch:
/// WebKit keeps the compiled form of a script or WASM module alongside the
/// cached response, and a response it is forbidden to store has nothing to keep
/// it alongside. `Gw.jspi.wasm` is 8.2 MB, and it was re-fetched, re-buffered
/// and re-compiled on every single boot.
///
/// `no-cache` rather than a long `max-age`, because these URLs are not
/// versioned: `patch::sync` rewrites `Gw.jspi.wasm` and `Gw.jspi.js` in place
/// whenever ArenaNet patches, under the same names. An `immutable` year would
/// pin a player to whichever client they first ran. `no-cache` permits the
/// store and requires the revalidation, which is exactly the pair wanted here —
/// and a revalidation that answers 304 keeps the compiled code.
fn respond_static(
    stream: &mut TcpStream,
    content_type: &str,
    body: &[u8],
    tag: Option<&str>,
) -> std::io::Result<()> {
    let extra: Vec<(&str, String)> = tag.map(|t| ("ETag", t.to_owned())).into_iter().collect();
    let head = format!(
        "HTTP/1.1 200 OK\r\n{}",
        common_headers(content_type, body.len() as u64, "no-cache", &extra)
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// The page already holds this exact file. A 304 carries no body by definition,
/// so the 8.2 MB stays on disk and so does its compiled form.
fn respond_not_modified(stream: &mut TcpStream, tag: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 304 Not Modified\r\n\
         Cache-Control: no-cache\r\n\
         ETag: {tag}\r\n\
         Content-Length: 0\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

/// A validator for a static file, from its length and modification time.
///
/// Not a digest of the contents: the point of the exercise is to stop reading
/// 8.2 MB on every launch, and hashing it to decide that would read it anyway.
/// Length and mtime both change when `patch::sync` replaces a file, which is
/// the only thing that ever changes one.
fn etag(meta: &std::fs::Metadata) -> Option<String> {
    let stamp = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(format!("\"{:x}-{:x}\"", meta.len(), stamp.as_nanos()))
}

/// Relay a proxied reply with upstream's own status and headers.
///
/// It does not go through `respond`: that one speaks for this host and states a
/// caching and cross-origin policy of its own, and neither is ours to assert on
/// behalf of an answer that came from somewhere else. Only the framing headers
/// are this hop's to write.
fn respond_proxy(stream: &mut TcpStream, reply: &proxy::Reply) -> std::io::Result<()> {
    // Clients key off the status code; the phrase is decoration and HTTP has
    // never required it to be the registered one.
    let mut head = format!(
        "HTTP/1.1 {} Proxied\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        // A header value carrying CRLF would let upstream inject headers of its
        // own choosing, or a whole second response, into this one.
        if value.contains(['\r', '\n']) {
            eprintln!("[proxy] dropped a header with embedded newlines: {name}");
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

fn respond_head(
    stream: &mut TcpStream,
    length: u64,
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\n{}",
        common_headers("application/octet-stream", length, "no-store", extra)
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOTAL: u64 = 1000;

    #[test]
    fn parses_closed_range() {
        assert_eq!(parse_range("bytes=0-99", TOTAL), Some((0, 99)));
    }

    #[test]
    fn open_ended_range_runs_to_the_end() {
        assert_eq!(parse_range("bytes=900-", TOTAL), Some((900, 999)));
    }

    #[test]
    fn suffix_range_counts_back_from_the_end() {
        assert_eq!(parse_range("bytes=-100", TOTAL), Some((900, 999)));
    }

    #[test]
    fn clamps_past_the_end() {
        assert_eq!(parse_range("bytes=500-99999", TOTAL), Some((500, 999)));
    }

    #[test]
    fn rejects_unsatisfiable_and_multi_ranges() {
        assert_eq!(parse_range("bytes=1000-1001", TOTAL), None);
        assert_eq!(parse_range("bytes=50-10", TOTAL), None);
        assert_eq!(parse_range("bytes=0-10,20-30", TOTAL), None);
        assert_eq!(parse_range("items=0-10", TOTAL), None);
    }

    #[test]
    fn resolve_refuses_traversal() {
        let root = std::env::temp_dir();
        assert!(resolve(&root, "../etc/passwd").is_none());
        assert!(resolve(&root, "/etc/passwd").is_none());
    }
}
