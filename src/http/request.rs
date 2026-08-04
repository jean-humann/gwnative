//! Reading one request off the wire.
//!
//! Nothing here decides anything or answers anything — it turns bytes into a
//! [`Request`] and says what that request asked for. The two subtleties worth
//! knowing about are both about the *next* request: a body left unread on a
//! kept-alive connection gets parsed as the following request line, and a body
//! too large to drain safely cannot be drained at all, so the caller closes.

use std::io::{self, BufRead, Read};

use base64::Engine as _;

/// A buffered reader that overwrites bytes as soon as `BufRead` consumes them.
pub struct WipingReader<R> {
    inner: R,
    buffer: Box<[u8; 8192]>,
    pos: usize,
    cap: usize,
}

impl<R> WipingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Box::new([0; 8192]),
            pos: 0,
            cap: 0,
        }
    }
}

impl<R: Read> Read for WipingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let available = self.fill_buf()?;
        let count = available.len().min(out.len());
        out[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl<R: Read> BufRead for WipingReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pos == self.cap {
            crate::log::wipe(&mut self.buffer[..self.cap]);
            self.pos = 0;
            self.cap = self.inner.read(&mut self.buffer[..])?;
        }
        Ok(&self.buffer[self.pos..self.cap])
    }

    fn consume(&mut self, amount: usize) {
        let end = (self.pos + amount).min(self.cap);
        crate::log::wipe(&mut self.buffer[self.pos..end]);
        self.pos = end;
    }
}

impl<R> Drop for WipingReader<R> {
    fn drop(&mut self) {
        crate::log::wipe(&mut self.buffer[..]);
    }
}

struct WipingString(String);

impl WipingString {
    fn new() -> Self {
        Self(String::new())
    }
}

impl std::ops::Deref for WipingString {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for WipingString {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for WipingString {
    fn drop(&mut self) {
        crate::log::wipe_string(&mut self.0);
    }
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub range: Option<String>,
    pub content_length: usize,
    /// Read through [`Request::wants_websocket`], which is the only question
    /// anything here asks of it.
    upgrade: Option<String>,
    pub websocket_key: Option<String>,
    websocket_protocol: Option<String>,
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

    /// The token the caller offered, by header or WebSocket subprotocol.
    ///
    /// The header is the right place for it and is what every `fetch` here
    /// uses. A browser WebSocket cannot set request headers, so it offers a
    /// prefixed capability as a subprotocol; unlike a query it never enters a
    /// URL, navigation history, or URL-bearing diagnostic.
    pub fn offered_token(&self) -> Option<&str> {
        self.token.as_deref().or_else(|| {
            self.websocket_protocol
                .as_deref()?
                .split(',')
                .find_map(|part| part.trim().strip_prefix("gwnative-token."))
        })
    }

    pub fn websocket_launch(&self) -> Option<crate::log::SecretText> {
        let encoded = self
            .websocket_protocol
            .as_deref()?
            .split(',')
            .find_map(|part| part.trim().strip_prefix("gwnative-launch."))?;
        let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()?;
        match String::from_utf8(std::mem::take(&mut bytes)) {
            Ok(value) => Some(crate::log::SecretText::new(value)),
            Err(error) => {
                let mut bytes = error.into_bytes();
                crate::log::wipe(&mut bytes);
                None
            }
        }
    }

    pub fn wants_websocket(&self) -> bool {
        self.upgrade
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            && self.websocket_key.is_some()
            && self
                .websocket_protocol
                .as_deref()
                .is_some_and(|protocols| protocols.split(',').any(|part| part.trim() == "gwnative"))
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        crate::log::wipe(&mut self.body);
        crate::log::wipe_string(&mut self.method);
        crate::log::wipe_string(&mut self.path);
        crate::log::wipe_string(&mut self.query);
        for value in [
            &mut self.range,
            &mut self.upgrade,
            &mut self.websocket_key,
            &mut self.websocket_protocol,
            &mut self.token,
            &mut self.if_none_match,
        ] {
            if let Some(value) = value.as_mut() {
                crate::log::wipe_string(value);
            }
        }
        for (name, value) in &mut self.headers {
            crate::log::wipe_string(name);
            crate::log::wipe_string(value);
        }
    }
}

/// Decoded in bytes throughout, never by slicing the `str`.
///
/// The query string arrives inside a line `read_line` has already proved is
/// UTF-8, and nothing says the character after a `%` is one byte wide. Slicing
/// `value[i + 1..i + 3]` to read the escape lands in the middle of any
/// multi-byte character that starts there, which is not a parse failure in Rust
/// but a panic — and this crate aborts on panic, so a query of `?token=%a€`
/// took the whole app down from any caller that could open a socket. A byte is
/// a hex digit or it is not; the width of the character it belongs to is not a
/// question this has to ask.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => match escape_at(bytes, i + 1) {
                Some(byte) => {
                    out.push(byte);
                    i += 3;
                }
                // A stray '%' is data, not a broken escape.
                None => {
                    out.push(b'%');
                    i += 1;
                }
            },
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
    match String::from_utf8(out) {
        Ok(decoded) => decoded,
        Err(error) => {
            let mut bytes = error.into_bytes();
            let decoded = String::from_utf8_lossy(&bytes).into_owned();
            crate::log::wipe(&mut bytes);
            decoded
        }
    }
}

/// The byte the two hex digits at `at` spell, if that is what they are.
///
/// `None` covers both ways a `%` can fail to begin an escape — too near the end
/// of the string, or not followed by two hex digits — because the caller treats
/// them the same. Stricter than the `from_str_radix` it replaces, which also
/// accepted a leading sign and so decoded `%+1` to a byte; RFC 3986 spells a
/// pct-encoded octet as `%` HEXDIG HEXDIG and nothing else.
fn escape_at(bytes: &[u8], at: usize) -> Option<u8> {
    let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let pair = bytes.get(at..at + 2)?;
    Some(digit(pair[0])? << 4 | digit(pair[1])?)
}

/// Largest body accepted on this server. Everything posted here is a log line,
/// a credential blob or a proxied API call; anything larger is a mistake, and on
/// a kept-alive connection an oversized body cannot simply be truncated.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// The request head is parsed before the token can be checked, so its budgets
/// are security boundaries rather than tuning knobs.
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADER_LINE_BYTES: usize = 16 * 1024;
const MAX_REQUEST_HEAD_BYTES: usize = 64 * 1024;
const MAX_HEADERS: usize = 100;

/// Read at most `limit` bytes of one line.
///
/// A line exactly as long as the limit is accepted only when its newline is
/// already among those bytes. Otherwise one more byte would make it oversized,
/// and the connection is going to close after the error so nothing needs to be
/// drained.
fn bounded_line(reader: &mut impl BufRead, line: &mut String, limit: usize) -> io::Result<usize> {
    crate::log::wipe_string(line);
    line.clear();
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP request head is too large",
        ));
    }
    let read = reader.take((limit + 1) as u64).read_line(line)?;
    if read > limit || (read == limit && !line.ends_with('\n')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP line is too large",
        ));
    }
    Ok(read)
}

/// Read one request, or `None` if the peer closed the connection first — which
/// on a kept-alive connection is the ordinary way an exchange ends, not a fault.
pub fn read_request(reader: &mut impl BufRead) -> std::io::Result<Option<Request>> {
    let mut line = WipingString::new();
    let mut head_bytes = bounded_line(reader, &mut line, MAX_REQUEST_LINE_BYTES)?;
    if head_bytes == 0 {
        return Ok(None);
    }

    // Construct the wiping owner before parsing anything sensitive so every
    // `?` and limit error below clears the copies accumulated so far.
    let mut request = Request {
        method: String::new(),
        path: String::new(),
        query: String::new(),
        range: None,
        content_length: 0,
        upgrade: None,
        websocket_key: None,
        websocket_protocol: None,
        token: None,
        if_none_match: None,
        headers: Vec::new(),
        body: Vec::new(),
        close: false,
    };
    let mut header = WipingString::new();
    loop {
        let remaining = MAX_REQUEST_HEAD_BYTES.saturating_sub(head_bytes);
        let read = bounded_line(reader, &mut header, MAX_HEADER_LINE_BYTES.min(remaining))?;
        head_bytes += read;
        if read == 0 || header.as_str() == "\r\n" || header.as_str() == "\n" {
            break;
        }
        if request.headers.len() == MAX_HEADERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many HTTP headers",
            ));
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_owned();
        match name.as_str() {
            "range" => request.range = Some(value.clone()),
            "content-length" => request.content_length = value.parse().unwrap_or(0),
            "upgrade" => request.upgrade = Some(value.clone()),
            "sec-websocket-key" => request.websocket_key = Some(value.clone()),
            "sec-websocket-protocol" => request.websocket_protocol = Some(value.clone()),
            "x-gwnative-token" => request.token = Some(value.clone()),
            "if-none-match" => request.if_none_match = Some(value.clone()),
            "connection" => request.close = value.eq_ignore_ascii_case("close"),
            _ => {}
        }
        request.headers.push((name, value));
    }

    let mut parts = line.split_whitespace();
    request.method = parts.next().unwrap_or("GET").to_owned();
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((path, rest)) => (path, rest.split('#').next().unwrap_or("")),
        None => (target.split('#').next().unwrap_or("/"), ""),
    };
    request.path = path.trim_start_matches('/').to_owned();
    request.query = query.to_owned();

    // A body larger than the cap cannot be drained safely — reading it is the
    // denial of service it would be refusing — so the caller closes instead.
    if request.content_length > 0 && request.content_length <= MAX_BODY_BYTES {
        request.body.resize(request.content_length, 0);
        reader.read_exact(&mut request.body)?;
    }

    Ok(Some(request))
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

/// Parse a single-span `bytes=` range into an inclusive, clamped `(start, end)`.
/// Multi-range requests are not supported and fall through as unsatisfiable.
pub fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    // Every arm below wants the last valid offset, and a resource of no length
    // does not have one — so it has no satisfiable range either, which is the
    // answer `None` already means here. Taken once, up front, because `total - 1`
    // written inline underflows: harmlessly in a release build, where the wrap
    // fails the bounds test on the last line anyway, but as a panic anywhere
    // overflow checks are on. `total` is a size out of the manifest, which is a
    // document fetched over the network, and none of the sizes in it are this
    // function's to vouch for.
    let last_offset = total.checked_sub(1)?;
    let (start, end) = match (first.trim(), last.trim()) {
        // A suffix range asks for the final N bytes.
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            (total.checked_sub(n)?, last_offset)
        }
        (first, "") => (first.parse().ok()?, last_offset),
        (first, last) => (
            first.parse().ok()?,
            last.parse::<u64>().ok()?.min(last_offset),
        ),
    };
    (start <= end && start < total).then_some((start, end))
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

    /// No byte of it exists, so no range over it is satisfiable — and reaching
    /// that answer must not go through `0 - 1` to get there.
    #[test]
    fn nothing_is_a_satisfiable_range_of_an_empty_resource() {
        for spec in ["bytes=0-", "bytes=0-0", "bytes=-0", "bytes=-10"] {
            assert_eq!(parse_range(spec, 0), None, "{spec} over an empty resource");
        }
    }

    /// The query string is UTF-8 — `read_line` would not have produced it
    /// otherwise — and a `%` in it can be followed by a character several bytes
    /// wide. Reading the escape by slicing the `str` lands inside that character
    /// and panics, which under this crate's `panic = "abort"` is the whole
    /// process. Anything that can open a socket to the loopback server can send
    /// this, so it remains covered even though capabilities no longer use a
    /// query parameter.
    fn query(q: &str) -> Request {
        let wire = format!("GET /__socket?{q} HTTP/1.1\r\n\r\n");
        read_request(&mut std::io::BufReader::new(wire.as_bytes()))
            .expect("a well-formed request line")
            .expect("not a closed connection")
    }

    #[test]
    fn a_multibyte_character_after_a_percent_is_data_not_a_crash() {
        assert_eq!(query("token=%a€").param("token").as_deref(), Some("%a€"));
        assert_eq!(query("token=%€x").param("token").as_deref(), Some("%€x"));
        assert_eq!(
            query("token=a%9éb").param("token").as_deref(),
            Some("a%9éb")
        );
        // The same shape at the very end of the string, where there are not two
        // bytes left to read at all.
        assert_eq!(query("token=abc%").param("token").as_deref(), Some("abc%"));
        assert_eq!(
            query("token=abc%4").param("token").as_deref(),
            Some("abc%4")
        );
    }

    #[test]
    fn a_real_escape_still_decodes() {
        assert_eq!(
            query("token=a%2Fb%2fc").param("token").as_deref(),
            Some("a/b/c")
        );
        assert_eq!(
            query("token=one+two").param("token").as_deref(),
            Some("one two")
        );
        // `from_str_radix` took a sign; a pct-encoded octet is two hex digits.
        assert_eq!(query("token=%+1").param("token").as_deref(), Some("% 1"));
        // The header still wins over the query string when both are offered.
        assert_eq!(
            query("token=fromquery").param("token").as_deref(),
            Some("fromquery")
        );
    }

    #[test]
    fn websocket_capability_comes_from_a_subprotocol_not_the_url() {
        let wire = b"GET /__socket?to=1.2.3.4%3A6112 HTTP/1.1\r\n\
            Upgrade: websocket\r\nSec-WebSocket-Key: key\r\n\
            Sec-WebSocket-Protocol: gwnative, gwnative-token.socket-canary, \
            gwnative-launch.eyJub25jZSI6ImV4YWN0LWxhdW5jaCJ9\r\n\r\n";
        let request = read_request(&mut std::io::BufReader::new(&wire[..]))
            .unwrap()
            .unwrap();
        assert_eq!(request.offered_token(), Some("socket-canary"));
        assert_eq!(
            request.websocket_launch().as_ref().map(AsRef::as_ref),
            Some(r#"{"nonce":"exact-launch"}"#)
        );
        assert!(!request.query.contains("socket-canary"));
        assert!(!request.query.contains("exact-launch"));
    }

    #[test]
    fn consumed_wire_bytes_are_overwritten_immediately() {
        let mut reader = WipingReader::new(std::io::Cursor::new(b"credential-canary"));
        let seen = reader.fill_buf().unwrap().len();
        assert!(seen > 0);
        reader.consume(seen);
        assert!(reader.buffer[..seen].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn the_request_head_has_hard_byte_and_field_limits() {
        let oversized_line = format!(
            "GET /{} HTTP/1.1\r\n\r\n",
            "x".repeat(MAX_REQUEST_LINE_BYTES)
        );
        let error = read_request(&mut std::io::BufReader::new(oversized_line.as_bytes()))
            .err()
            .expect("the request line is over budget");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let many_headers = format!(
            "GET / HTTP/1.1\r\n{}\r\n",
            "X-Test: value\r\n".repeat(MAX_HEADERS + 1)
        );
        let error = read_request(&mut std::io::BufReader::new(many_headers.as_bytes()))
            .err()
            .expect("the request has too many headers");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let oversized_head = format!(
            "GET / HTTP/1.1\r\n{}\r\n",
            format!("X-Fill: {}\r\n", "x".repeat(MAX_HEADER_LINE_BYTES - 10))
                .repeat(MAX_REQUEST_HEAD_BYTES / MAX_HEADER_LINE_BYTES + 1)
        );
        let error = read_request(&mut std::io::BufReader::new(oversized_head.as_bytes()))
            .err()
            .expect("the request head is over budget");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
