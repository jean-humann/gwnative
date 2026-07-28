//! Writing one reply back.
//!
//! Every response this host speaks for carries the same head — the caching,
//! cross-origin and content-type policy in [`common_headers`] — because a
//! response that quietly omits one of them is indistinguishable from one that
//! carries it until something breaks in the page. The single exception is
//! [`respond_proxy`], which relays an answer from somewhere else and so has no
//! business asserting this host's policy on its behalf.

// Anonymous because `io::Write` is here too and both spell `write!`; the
// compiler picks by receiver, and only strings use this one.
use std::fmt::Write as _;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};

use crate::proxy;

/// How much of a streamed body to gather before it goes to the socket.
///
/// A range is served chunk-window by chunk-window, and the commonest of those is
/// 472 bytes; handing each one to `write` separately would be a syscall per
/// window with `TCP_NODELAY` sending each as its own segment.
pub const BODY_BUFFER: usize = 64 * 1024;

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
         Cross-Origin-Resource-Policy: same-origin\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: {}\r\n",
        POLICY.get().map_or("default-src 'self'", String::as_str)
    );
    for (name, value) in extra {
        // Written in place rather than through a `format!`, which would
        // allocate a String per header and drop it a line later.
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    head
}

/// The reason phrase for a status code this server sends.
///
/// Clients key off the number and no browser has read the phrase in twenty
/// years, so this exists for whoever is reading a packet capture. Derived here
/// rather than passed in because a call site that spells its own phrase is a
/// call site that can answer 403 with "Not Found" — there were thirty-two of
/// them, and nothing would have caught the pair drifting apart.
fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        426 => "Upgrade Required",
        502 => "Bad Gateway",
        507 => "Insufficient Storage",
        _ => "Status",
    }
}

pub fn respond(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    respond_streaming(stream, code, content_type, body.len() as u64, extra)?;
    stream.write_all(body)?;
    stream.flush()
}

/// The head of a response whose body is written separately. Used where the body
/// is streamed as it is produced rather than assembled first — and by
/// [`respond`], whose body simply happens to be ready.
pub fn respond_streaming(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    length: u64,
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {}\r\n{}",
        reason(code),
        common_headers(content_type, length, "no-store", extra)
    );
    stream.write_all(head.as_bytes())
}

/// A short plain-text answer, which is what most refusals here are.
pub fn text(stream: &mut TcpStream, code: u16, message: &str) -> std::io::Result<()> {
    respond(stream, code, "text/plain", message.as_bytes(), &[])
}

/// Done, with nothing to say about it.
pub fn no_content(stream: &mut TcpStream) -> std::io::Result<()> {
    respond(stream, 204, "text/plain", b"", &[])
}

pub fn json(stream: &mut TcpStream, code: u16, body: &[u8]) -> std::io::Result<()> {
    respond(stream, code, "application/json", body, &[])
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
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

/// A HEAD: the head a GET would have carried, and no body by definition.
pub fn respond_head(
    stream: &mut TcpStream,
    length: u64,
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    respond_streaming(stream, 200, "application/octet-stream", length, extra)?;
    stream.flush()
}
