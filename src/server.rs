//! Loopback origin for the harness.
//!
//! The client has to run in a secure context (IndexedDB, crypto.subtle) and the
//! harness wants microsecond timers. A custom WKURLSchemeHandler origin gets the
//! secure context but WebKit clamps `performance.now()` there to 1 ms, which is
//! too coarse for the per-frame telemetry. A loopback origin is trustworthy per
//! spec, is not clamped, and — with COOP/COEP — is cross-origin isolated, so
//! SharedArrayBuffer stays available if the client ever wants threads.
//!
//! Bound to 127.0.0.1 on an ephemeral port, so nothing is reachable off-host.
//!
//! `Gw.snapshot` is served from here too, as a virtual ranged file. It is 4.2 GB
//! and the client reads a small fraction of it per session, so the bytes come
//! from the chunk store on demand rather than from disk.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;

use crate::chunks::ChunkStore;
use crate::patch::SNAPSHOT;
use crate::sockets::{self, Registry};
use crate::{keychain, net, proxy, ws};

/// Largest span served from one request. The harness asks for far less; this
/// only stops a stray `Range: bytes=0-` from trying to buffer the whole image.
const MAX_RANGE_BYTES: u64 = 32 * 1024 * 1024;

pub struct Loopback {
    pub addr: SocketAddr,
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
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
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
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let context = Arc::clone(&context);
                // One thread per connection. Snapshot reads block on the chunk
                // store, so they must not share a thread with the page load.
                thread::spawn(move || {
                    let _ = serve(stream, &context);
                });
            }
        })?;

    Ok(Loopback { addr })
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
    /// Every header, lower-cased, in arrival order. Only the proxy needs them:
    /// it has to forward what the page sent rather than a reconstruction of it.
    headers: Vec<(String, String)>,
}

impl Request {
    /// First value for `name` in the query string, percent-decoded.
    fn param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| percent_decode(value))
        })
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

fn read_request(reader: &mut impl BufRead) -> std::io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let mut range = None;
    let mut content_length = 0usize;
    let mut upgrade = None;
    let mut websocket_key = None;
    let mut token = None;
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

    Ok(Request {
        method,
        path: path.trim_start_matches('/').to_owned(),
        query: query.to_owned(),
        range,
        content_length,
        upgrade,
        websocket_key,
        token,
        headers,
    })
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

fn serve(mut stream: TcpStream, context: &Context) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = read_request(&mut reader)?;

    // Diagnostic channel for the bring-up harness. Real host calls will go over
    // WKScriptMessageHandlerWithReply; this only exists so a page can report
    // results without a UI.
    if request.method == "POST" && request.path == "__report" {
        let mut body = vec![0u8; request.content_length.min(1 << 20)];
        reader.read_exact(&mut body)?;
        eprintln!("[report] {}", String::from_utf8_lossy(&body));
        return respond(&mut stream, 204, "No Content", "text/plain", b"", &[]);
    }

    // The game asks for an address before it dials. Answering here keeps name
    // resolution on the host, where the public-unicast policy lives.
    if request.path == "__dns" {
        let name = request.param("name").unwrap_or_default();
        return match net::resolve(&name) {
            Ok(address) => {
                eprintln!("[dns] {name} -> {address}");
                respond(
                    &mut stream,
                    200,
                    "OK",
                    "text/plain",
                    address.to_string().as_bytes(),
                    &[],
                )
            }
            Err(e) => {
                eprintln!("[dns] {name}: {e}");
                respond(
                    &mut stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    e.to_string().as_bytes(),
                    &[],
                )
            }
        };
    }

    // Saved login. Gated on the injected token; see `spawn`.
    if request.path == "__credentials" {
        if !token_matches(&context.token, request.token.as_deref()) {
            eprintln!(
                "[credentials] refused an untokened {} request",
                request.method
            );
            return respond(
                &mut stream,
                403,
                "Forbidden",
                "text/plain",
                b"forbidden",
                &[],
            );
        }
        return match request.method.as_str() {
            "GET" => match keychain::load() {
                Some(credentials) => {
                    let body = serde_json::to_vec(&credentials).unwrap_or_default();
                    eprintln!("[credentials] read from the keychain");
                    respond(&mut stream, 200, "OK", "application/json", &body, &[])
                }
                // Not an error: a first launch has nothing saved, and the client
                // treats "none" as "ask the player".
                None => {
                    eprintln!("[credentials] nothing saved yet");
                    respond(
                        &mut stream,
                        404,
                        "Not Found",
                        "text/plain",
                        b"no stored credentials",
                        &[],
                    )
                }
            },
            "PUT" => {
                let mut body = vec![0u8; request.content_length.min(64 * 1024)];
                reader.read_exact(&mut body)?;
                let stored = serde_json::from_slice(&body)
                    .map_err(|e| e.to_string())
                    .and_then(|c: keychain::Credentials| keychain::store(&c));
                match stored {
                    Ok(()) => {
                        eprintln!("[credentials] saved to the keychain");
                        respond(&mut stream, 204, "No Content", "text/plain", b"", &[])
                    }
                    Err(e) => {
                        eprintln!("[credentials] not saved: {e}");
                        respond(
                            &mut stream,
                            400,
                            "Bad Request",
                            "text/plain",
                            e.as_bytes(),
                            &[],
                        )
                    }
                }
            }
            "DELETE" => {
                keychain::clear();
                eprintln!("[credentials] cleared");
                respond(&mut stream, 204, "No Content", "text/plain", b"", &[])
            }
            _ => respond(
                &mut stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"use GET, PUT or DELETE",
                &[("Allow", "GET, PUT, DELETE".into())],
            ),
        };
    }

    if request.path == "__socket" {
        if !request.wants_websocket() {
            return respond(
                &mut stream,
                426,
                "Upgrade Required",
                "text/plain",
                b"__socket is a websocket endpoint",
                &[("Upgrade", "websocket".into())],
            );
        }
        let destination = request.param("to").unwrap_or_default();
        ws::accept(&mut stream, request.websocket_key.as_deref().unwrap_or(""))?;
        // bridge() owns the connection from here — it is no longer HTTP. The
        // reader goes with it: anything the peer sent after the request head is
        // already inside its buffer, and a fresh reader would drop those bytes.
        sockets::bridge(reader, stream, &destination, &context.sockets);
        return Ok(());
    }

    if request.path == "__stats"
        && let Some(store) = &context.snapshot
    {
        let (cache, net, coalesced) = store.stats();
        let body = format!(r#"{{"fromCache":{cache},"fetched":{net},"coalesced":{coalesced}}}"#);
        return respond(
            &mut stream,
            200,
            "OK",
            "application/json",
            body.as_bytes(),
            &[],
        );
    }

    if request.path == "__resident"
        && let Some(store) = &context.snapshot
    {
        let bits = store.resident_bitmap();
        return respond(
            &mut stream,
            200,
            "OK",
            "application/octet-stream",
            &bits,
            &[("X-Chunk-Size", store.chunk_size().to_string())],
        );
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
        return respond(
            &mut stream,
            200,
            "OK",
            "application/json",
            body.as_bytes(),
            &[],
        );
    }

    if request.path == SNAPSHOT
        && let Some(store) = &context.snapshot
    {
        return serve_snapshot(&mut stream, &request, store);
    }

    // The client's own web requests, which it addressed to this origin because
    // its glue rewrote them. See `proxy` for why, and why the table is closed.
    let (route, tail) = match request.path.split_once('/') {
        Some((route, tail)) => (route, format!("/{tail}")),
        None => (request.path.as_str(), "/".to_owned()),
    };
    if proxy::host(route).is_some() {
        let mut body = Vec::new();
        if request.method != "GET" {
            if request.content_length > proxy::MAX_BODY {
                return respond(
                    &mut stream,
                    413,
                    "Payload Too Large",
                    "text/plain",
                    b"request body too large",
                    &[],
                );
            }
            body.resize(request.content_length, 0);
            reader.read_exact(&mut body)?;
        }
        return match proxy::forward(
            route,
            &tail,
            &request.query,
            &request.method,
            &request.headers,
            body,
        ) {
            Ok(reply) => {
                eprintln!(
                    "[proxy] {} /{route}{tail} -> {}",
                    request.method, reply.status
                );
                respond_proxy(&mut stream, &reply)
            }
            Err(e) => {
                eprintln!("[proxy] {} /{route}{tail}: {e}", request.method);
                respond(
                    &mut stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    b"proxy error",
                    &[],
                )
            }
        };
    }

    match resolve(&context.root, &request.path) {
        Some(file) => match std::fs::read(&file) {
            Ok(body) => {
                eprintln!("[loopback] 200 /{} ({} bytes)", request.path, body.len());
                respond(&mut stream, 200, "OK", mime(&file), &body, &[])
            }
            Err(_) => {
                eprintln!("[loopback] 404 /{}", request.path);
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "text/plain",
                    b"not found",
                    &[],
                )
            }
        },
        None => {
            eprintln!("[loopback] 403 /{}", request.path);
            respond(
                &mut stream,
                403,
                "Forbidden",
                "text/plain",
                b"forbidden",
                &[],
            )
        }
    }
}

fn serve_snapshot(
    stream: &mut TcpStream,
    request: &Request,
    store: &ChunkStore,
) -> std::io::Result<()> {
    let total = store.snapshot_size();

    // A HEAD is how the client learns the image size without pulling a byte.
    if request.method == "HEAD" {
        return respond_head(stream, total, &[("Accept-Ranges", "bytes".into())]);
    }

    let Some((start, end)) = request.range.as_deref().and_then(|r| parse_range(r, total)) else {
        // Refusing an unranged GET is deliberate: answering it means streaming
        // 4.2 GB, which is never what the client wants.
        return respond(
            stream,
            416,
            "Range Not Satisfiable",
            "text/plain",
            b"snapshot requires a byte range",
            &[("Content-Range", format!("bytes */{total}"))],
        );
    };

    let length = (end - start + 1).min(MAX_RANGE_BYTES);
    match store.read(start, length) {
        Ok(body) => {
            let last = start + body.len() as u64 - 1;
            eprintln!(
                "[loopback] 206 /{SNAPSHOT} bytes {start}-{last}/{total} ({} bytes)",
                body.len()
            );
            respond(
                stream,
                206,
                "Partial Content",
                "application/octet-stream",
                &body,
                &[
                    ("Content-Range", format!("bytes {start}-{last}/{total}")),
                    ("Accept-Ranges", "bytes".into()),
                ],
            )
        }
        Err(e) => {
            eprintln!("[loopback] 502 /{SNAPSHOT} {start}+{length}: {e}");
            respond(
                stream,
                502,
                "Bad Gateway",
                "text/plain",
                e.to_string().as_bytes(),
                &[],
            )
        }
    }
}

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
fn common_headers(content_type: &str, length: u64, extra: &[(&str, String)]) -> String {
    let mut head = format!(
        "Content-Type: {content_type}\r\n\
         Content-Length: {length}\r\n\
         Cache-Control: no-store\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Connection: close\r\n"
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
        common_headers(content_type, body.len() as u64, extra)
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
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
        common_headers("application/octet-stream", length, extra)
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
