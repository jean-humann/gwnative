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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::thread;

pub struct Loopback {
    pub addr: SocketAddr,
}

/// Serve `root` on 127.0.0.1:<ephemeral> until the process exits.
pub fn spawn(root: PathBuf) -> std::io::Result<Loopback> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    let addr = listener.local_addr()?;

    thread::Builder::new()
        .name("gwnative-loopback".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let root = root.clone();
                // One thread per connection is fine at this volume; the harness
                // fetches a handful of files and the game data does not come
                // through here.
                thread::spawn(move || {
                    let _ = serve(stream, &root);
                });
            }
        })?;

    Ok(Loopback { addr })
}

fn serve(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let mut content_length = 0usize;
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(v) = header
            .strip_prefix("Content-Length:")
            .or_else(|| header.strip_prefix("content-length:"))
        {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");

    // Diagnostic channel for the bring-up harness. Real host calls will go over
    // WKScriptMessageHandlerWithReply; this only exists so a page can report
    // results without a UI.
    if method == "POST" && path == "/__report" {
        let mut body = vec![0u8; content_length.min(1 << 20)];
        reader.read_exact(&mut body)?;
        eprintln!("[report] {}", String::from_utf8_lossy(&body));
        return respond(&mut stream, 204, "No Content", "text/plain", b"");
    }

    match resolve(root, path) {
        Some(file) => match std::fs::read(&file) {
            Ok(body) => {
                eprintln!("[loopback] 200 {path} ({} bytes)", body.len());
                respond(&mut stream, 200, "OK", mime(&file), &body)
            }
            Err(_) => {
                eprintln!("[loopback] 404 {path}");
                respond(&mut stream, 404, "Not Found", "text/plain", b"not found")
            }
        },
        None => {
            eprintln!("[loopback] 403 {path}");
            respond(&mut stream, 403, "Forbidden", "text/plain", b"forbidden")
        }
    }
}

/// Reject anything that escapes `root`. `..` and absolute components are the
/// whole attack surface of a static file server, so they are refused outright
/// rather than normalised.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

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

fn respond(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Cache-Control: no-store\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Connection: close\r\n\r\n",
        len = body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}
