//! Reading one request off the wire.
//!
//! Nothing here decides anything or answers anything — it turns bytes into a
//! [`Request`] and says what that request asked for. The two subtleties worth
//! knowing about are both about the *next* request: a body left unread on a
//! kept-alive connection gets parsed as the following request line, and a body
//! too large to drain safely cannot be drained at all, so the caller closes.

use std::io::BufRead;

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
}
