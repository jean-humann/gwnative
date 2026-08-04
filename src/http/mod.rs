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
//! it answers with, is next door. [`request`] does the reading and [`response`]
//! the writing, and what is left here is the pair in between: which file on
//! disk a request path names, and what content type that file is answered with.

mod request;
mod response;

use std::path::{Component, Path, PathBuf};

pub use request::{
    MAX_BODY_BYTES, Request, WipingReader, parse_range, read_request, token_matches,
};
pub use response::{
    BODY_BUFFER, POLICY, etag, json, no_content, policy, respond, respond_head,
    respond_not_modified, respond_proxy, respond_static, respond_streaming, text,
};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_refuses_traversal() {
        let root = std::env::temp_dir();
        assert!(resolve(&root, "../etc/passwd").is_none());
        assert!(resolve(&root, "/etc/passwd").is_none());
    }
}
