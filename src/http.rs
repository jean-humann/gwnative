//! HTTP, as much of it as the loopback origin needs.
//!
//! One page talks to this server and it is a page this repository ships, so
//! "as much as it needs" is a short list: one request at a time, no chunked
//! encoding, one range span, no content negotiation. What is here instead of a
//! crate is the parts that are easy to get subtly wrong — a body left unread
//! becoming the next request line, a range that clamps past the end, a token
//! compared in time that depends on how many bytes matched.
//!
//! Split out of `server.rs` because none of it decides anything. It reads
//! bytes into a `Request` and writes bytes back; which route answers, and what
//! it answers with, is next door.

use std::io::{BufRead, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Component, Path, PathBuf};

use crate::proxy;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub range: Option<String>,
    pub content_length: usize,
    pub upgrade: Option<String>,
    pub websocket_key: Option<String>,
    pub token: Option<String>,
    /// The validator the page already holds for a static file, if any.
    pub if_none_match: Option<String>,
    /// Every header, lower-cased, in arrival order. Only the proxy needs them:
    /// it has to forward what the page sent rather than a reconstruction of it.
    pub headers: Vec<(String, String)>,
    /// Read up front rather than by whichever handler wants it. On a kept-alive
    /// connection a body left unread is not merely ignored — it sits in the
    /// stream and gets parsed as the next request line.
    pub body: Vec<u8>,
    /// The peer asked to close after this exchange.
    pub close: bool,
}

impl Request {
    /// First value for `name` in the query string, percent-decoded.
    pub fn param(&self, name: &str) -> Option<String> {
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
    pub fn offered_token(&self) -> Option<String> {
        self.token.clone().or_else(|| self.param("token"))
    }

    pub fn wants_websocket(&self) -> bool {
        self.upgrade
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            && self.websocket_key.is_some()
    }
}

pub fn percent_decode(value: &str) -> String {
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
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Read one request, or `None` if the peer closed the connection first — which
/// on a kept-alive connection is the ordinary way an exchange ends, not a fault.
pub fn read_request(reader: &mut impl BufRead) -> std::io::Result<Option<Request>> {
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
pub fn token_matches(expected: &str, offered: Option<&str>) -> bool {
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

/// How much of a streamed body to gather before it goes to the socket.
///
/// A range is served chunk-window by chunk-window, and the commonest of those is
/// 472 bytes; handing each one to `write` separately would be a syscall per
/// window with `TCP_NODELAY` sending each as its own segment.
pub const BODY_BUFFER: usize = 64 * 1024;

/// Parse a single-span `bytes=` range into an inclusive, clamped `(start, end)`.
/// Multi-range requests are not supported and fall through as unsatisfiable.
pub fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
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
pub fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
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

pub fn mime(path: &Path) -> &'static str {
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

/// The policy every response carries, once the origin's address is known.
///
/// A `OnceLock` rather than a constant because `connect-src` names the
/// WebSocket origin explicitly. `'self'` ought to cover `ws://` to the same
/// host and port per CSP3, but the port here is configurable and a policy that
/// silently stops the game talking to its own sockets is not worth the two
/// tokens it saves.
pub static POLICY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// What the client's own JavaScript needs, and nothing more.
///
/// The two `unsafe` grants are not decoration. Emscripten glue calls `new
/// Function` on generated source for its dynamic call thunks, so `'unsafe-eval'`
/// is the difference between the game booting and not; `'wasm-unsafe-eval'` is
/// what permits `WebAssembly.instantiate` at all. `worker-src blob:` is there
/// for the same reason: the glue spawns its workers from object URLs.
///
/// What the policy actually buys is everything it does *not* list. No `img-src`
/// beyond the origin and `data:`, no `frame-src` at all, `object-src 'none'`,
/// `base-uri 'none'` so nothing can retarget every relative URL on the page, and
/// `form-action 'none'` so a form cannot post anywhere. Those close the routes
/// by which injected content in an 8.2 MB third-party module could reach off the
/// machine — which is the exposure worth closing, since the module itself has to
/// be run as it was shipped.
pub fn policy(addr: SocketAddr) -> String {
    format!(
        "default-src 'self'; \
         script-src 'self' 'unsafe-eval' 'wasm-unsafe-eval'; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; \
         font-src 'self'; \
         connect-src 'self' ws://{addr} blob: data:; \
         worker-src 'self' blob:; \
         object-src 'none'; \
         base-uri 'none'; \
         frame-src 'none'; \
         form-action 'none'; \
         frame-ancestors 'none'"
    )
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
pub fn common_headers(
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
         Cross-Origin-Resource-Policy: same-origin\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: {}\r\n",
        POLICY.get().map_or("default-src 'self'", String::as_str)
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    head
}

pub fn respond(
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
pub fn respond_static(
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
pub fn respond_not_modified(stream: &mut TcpStream, tag: &str) -> std::io::Result<()> {
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
pub fn etag(meta: &std::fs::Metadata) -> Option<String> {
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
pub fn respond_proxy(stream: &mut TcpStream, reply: &proxy::Reply) -> std::io::Result<()> {
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
            note!("[proxy] dropped a header with embedded newlines: {name}");
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

pub fn respond_head(
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
