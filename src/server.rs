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

/// Largest span served from one request. The harness asks for far less; this
/// only stops a stray `Range: bytes=0-` from trying to buffer the whole image.
const MAX_RANGE_BYTES: u64 = 32 * 1024 * 1024;

pub struct Loopback {
    pub addr: SocketAddr,
}

struct Context {
    root: PathBuf,
    snapshot: Option<Arc<ChunkStore>>,
}

/// Serve `root` on 127.0.0.1:<ephemeral> until the process exits.
pub fn spawn(root: PathBuf, snapshot: Option<Arc<ChunkStore>>) -> std::io::Result<Loopback> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    let addr = listener.local_addr()?;
    let context = Arc::new(Context { root, snapshot });

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
    range: Option<String>,
    content_length: usize,
}

fn read_request(reader: &mut impl BufRead) -> std::io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let mut range = None;
    let mut content_length = 0usize;
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        match name.to_ascii_lowercase().as_str() {
            "range" => range = Some(value),
            "content-length" => content_length = value.parse().unwrap_or(0),
            _ => {}
        }
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let target = parts.next().unwrap_or("/");
    let path = target
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_owned();

    Ok(Request {
        method,
        path,
        range,
        content_length,
    })
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

    if request.path == SNAPSHOT
        && let Some(store) = &context.snapshot
    {
        return serve_snapshot(&mut stream, &request, store);
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
