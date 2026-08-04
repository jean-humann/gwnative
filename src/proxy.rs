//! The client's own web requests, forwarded to ArenaNet and NCSoft.
//!
//! ArenaNet's glue does not dial these hosts. Outside Capacitor it folds every
//! outbound request onto the page's own origin, under a path equal to the first
//! label of the host it meant to reach:
//!
//! ```text
//! https://webgate.ncplatform.net/foo  ->  <origin>/webgate/foo
//! ```
//!
//! so a host that does not put those five names back answers 404 to the login
//! request itself — logging in is an HTTPS call to NCSoft's gateway, not a packet
//! on the game socket. That failure surfaces as "unable to connect to any login
//! servers", which reads like a network fault and is not one.
//!
//! The table is an allowlist rather than a rule for turning a label back into a
//! domain. The page can put any path it likes in front of us, and this is the one
//! place in the host that will open a connection to an arbitrary name on demand.

use std::time::Duration;

use crate::transport;

/// The five hosts the client reaches for. Each key is the first label of its
/// value, because that is all the glue keeps when it rewrites the URL.
const ROUTES: [(&str, &str); 5] = [
    ("account", "account.arena.net"),
    ("help", "help.guildwars.com"),
    ("store", "store.guildwars.com"),
    ("webgate", "webgate.ncplatform.net"),
    ("www", "www.guildwars.com"),
];

/// Login is interactive, so a stalled request has to fail while the player is
/// still looking at the spinner.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Generous for a form post and far under anything that would strain the host.
pub const MAX_BODY: usize = 8 * 1024 * 1024;

/// Sent by the browser or meaningful only to the hop that received them.
/// `x-gwnative-token` is ours and authorises reading the saved password: it has
/// no business upstream even though the page has no reason to send it.
const REQUEST_DROP: &[&str] = &[
    "connection",
    "content-length",
    "cookie",
    "host",
    "keep-alive",
    "origin",
    "proxy-connection",
    "referer",
    "sec-websocket-extensions",
    "sec-websocket-key",
    "sec-websocket-protocol",
    "sec-websocket-version",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "x-gwnative-token",
];

/// Upstream's transport framing describes its connection, not ours, and its
/// content policy describes its origin, not the page's.
const RESPONSE_DROP: &[&str] = &[
    "connection",
    "content-encoding",
    "content-length",
    "content-security-policy",
    "content-security-policy-report-only",
    "keep-alive",
    "proxy-connection",
    "sec-websocket-extensions",
    "sec-websocket-protocol",
    "set-cookie",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "x-content-type-options",
];

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Drop for Reply {
    fn drop(&mut self) {
        for (name, value) in &mut self.headers {
            crate::log::wipe_string(name);
            crate::log::wipe_string(value);
        }
        crate::log::wipe(&mut self.body);
    }
}

fn drop_header(name: &str, blocked: &[&str]) -> bool {
    blocked.iter().any(|item| name.eq_ignore_ascii_case(item))
}

fn upstream_request_headers(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    let mut forwarded = headers
        .iter()
        .filter(|(name, _)| !drop_header(name, REQUEST_DROP))
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    forwarded.push(("accept-encoding", "identity"));
    forwarded
}

/// The host a route label stands for, or `None` if it is not one of the five.
pub fn host(route: &str) -> Option<&'static str> {
    ROUTES
        .iter()
        .find(|(name, _)| *name == route)
        .map(|(_, host)| *host)
}

/// Forward one request. `tail` is the path after the route label, leading slash
/// included; `query` is the raw query string without its `?`.
pub fn forward(
    route: &str,
    tail: &str,
    query: &str,
    method: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Reply, String> {
    let host = host(route).ok_or_else(|| "unknown proxy route".to_owned())?;
    if !matches!(method, "GET" | "POST" | "PUT") {
        return Err("method not allowed".into());
    }
    let mut url = if query.is_empty() {
        format!("https://{host}{tail}")
    } else {
        format!("https://{host}{tail}?{query}")
    };

    // Identity last so it wins if the page sent an accept-encoding of its own:
    // the bodies here are already compressed or small, and decoding one
    // transfer layer only to re-frame it below buys nothing.
    let forwarded = upstream_request_headers(headers);

    // Redirects are not followed — the transport hands the 3xx back — so a
    // redirect that leaves the allowlisted host is refused below instead of
    // quietly fetched. A 401 is an answer the client knows how to render; it
    // arrives here as a status like any other and reaches the page intact.
    let parts = std::iter::once(url.as_bytes())
        .chain(
            forwarded
                .iter()
                .flat_map(|(name, value)| [name.as_bytes(), value.as_bytes()]),
        )
        .chain(std::iter::once(body));
    let Some(lease) = crate::log::admit_untrusted_parts(parts) else {
        crate::log::wipe_string(&mut url);
        return Err("protected request material was not forwarded".into());
    };
    let response = transport::fetch(
        method,
        &url,
        &forwarded,
        matches!(method, "POST" | "PUT").then_some(body),
        TIMEOUT,
    );
    drop(lease);
    crate::log::wipe_string(&mut url);
    let mut response = match response {
        Ok(response) => response,
        Err(mut error) => {
            crate::log::wipe_string(&mut error);
            return Err("upstream request failed".into());
        }
    };

    let mut out = Vec::new();
    for (mut name, mut value) in std::mem::take(&mut response.headers) {
        if drop_header(&name, RESPONSE_DROP) {
            crate::log::wipe_string(&mut name);
            crate::log::wipe_string(&mut value);
            continue;
        }
        if name == "location" {
            match rewrite_location(route, host, &value) {
                Some(safe) => out.push((name, safe)),
                // Dropping the header turns the redirect into a bare 3xx, which
                // the client reports as a failed request rather than following
                // somewhere this host never vouched for.
                None => note!("[proxy] {route}: refused a redirect to {value}"),
            }
            crate::log::wipe_string(&mut value);
            continue;
        }
        out.push((name, value));
    }

    if response.body.len() > MAX_BODY {
        for (name, value) in &mut out {
            crate::log::wipe_string(name);
            crate::log::wipe_string(value);
        }
        crate::log::wipe(&mut response.body);
        return Err(format!("response body over {MAX_BODY} bytes"));
    }

    Ok(Reply {
        status: response.status,
        headers: out,
        body: std::mem::take(&mut response.body),
    })
}

/// Point a redirect back at this origin, or refuse it.
///
/// Only same-host targets survive. A redirect off the allowlisted host is how a
/// compromised or merely over-helpful endpoint would turn this proxy into an
/// open one, so it is refused rather than followed or passed through verbatim.
fn rewrite_location(route: &str, host: &str, location: &str) -> Option<String> {
    // `//host/path` is protocol-relative and names a different authority.
    if location.starts_with('/') && !location.starts_with("//") {
        return Some(format!("/{route}{location}"));
    }
    // Plain http would downgrade a login flow, so only https is accepted. A
    // path-relative target is refused rather than resolved: these endpoints do
    // not send one, and guessing at a base is how a bypass gets built.
    let rest = location.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // Userinfo can spell one authority and mean another.
    if authority.contains('@') {
        return None;
    }
    let (name, port) = match authority.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (authority, None),
    };
    if !name.eq_ignore_ascii_case(host) || port.is_some_and(|port| port != "443") {
        return None;
    }
    let tail = &rest[end..];
    Some(format!(
        "/{route}{}",
        if tail.is_empty() { "/" } else { tail }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_labels_to_hosts() {
        assert_eq!(host("webgate"), Some("webgate.ncplatform.net"));
        assert_eq!(host("account"), Some("account.arena.net"));
        assert_eq!(host("evil"), None);
    }

    #[test]
    fn every_route_is_the_first_label_of_its_host() {
        // The glue derives the label with `hostname.split('.')[0]`, so a table
        // entry that disagrees would never be reached.
        for (route, host) in ROUTES {
            assert_eq!(host.split('.').next(), Some(route));
        }
    }

    #[test]
    fn cookie_state_never_crosses_the_five_route_proxy() {
        let offered = [
            ("Cookie".into(), "a=1".into()),
            ("cOoKiE".into(), "b=2".into()),
            ("X-Gwnative-Token".into(), "sentinel".into()),
            (
                "sEc-WeBsOcKeT-pRoToCoL".into(),
                "gwnative-token.admin".into(),
            ),
            ("Upgrade".into(), "websocket".into()),
            ("Sec-WebSocket-Key".into(), "key".into()),
            ("accept".into(), "application/json".into()),
        ];
        for route in ["account", "webgate"] {
            assert!(host(route).is_some());
            assert_eq!(
                upstream_request_headers(&offered),
                [
                    ("accept", "application/json"),
                    ("accept-encoding", "identity")
                ]
            );
        }
        for name in ["set-cookie", "Set-Cookie", "sEt-CoOkIe"] {
            assert!(drop_header(name, RESPONSE_DROP));
        }
        assert!(!drop_header("set-cookie", REQUEST_DROP));
        assert!(!drop_header("cookie", RESPONSE_DROP));
    }

    #[test]
    fn keeps_same_host_redirects_on_this_origin() {
        let host = "webgate.ncplatform.net";
        assert_eq!(
            rewrite_location("webgate", host, "/login?next=1").as_deref(),
            Some("/webgate/login?next=1")
        );
        assert_eq!(
            rewrite_location("webgate", host, "https://webgate.ncplatform.net/a/b").as_deref(),
            Some("/webgate/a/b")
        );
        assert_eq!(
            rewrite_location("webgate", host, "https://webgate.ncplatform.net:443/a").as_deref(),
            Some("/webgate/a")
        );
        assert_eq!(
            rewrite_location("webgate", host, "https://WEBGATE.ncplatform.NET/a").as_deref(),
            Some("/webgate/a")
        );
    }

    #[test]
    fn refuses_redirects_that_leave_the_host() {
        let host = "webgate.ncplatform.net";
        for location in [
            "https://evil.example/",
            "http://webgate.ncplatform.net/a",
            "//evil.example/a",
            "https://webgate.ncplatform.net.evil.example/a",
            "https://evil.example@webgate.ncplatform.net/a",
            "https://webgate.ncplatform.net:8443/a",
            "relative/path",
        ] {
            assert_eq!(
                rewrite_location("webgate", host, location),
                None,
                "{location}"
            );
        }
    }

    #[test]
    fn protected_request_material_is_refused_before_network_io() {
        let secret = "z9Q";
        let _registration = crate::log::register(&[secret]).unwrap();
        for result in [
            forward("webgate", "/z9Q", "", "GET", &[], b""),
            forward("webgate", "/", "v=%7a%39%51", "GET", &[], b""),
            forward(
                "webgate",
                "/",
                "",
                "GET",
                &[("x-canary".into(), secret.into())],
                b"",
            ),
            forward("webgate", "/", "", "POST", &[], br#"{"password":"z9Q"}"#),
        ] {
            assert_eq!(
                result.err().as_deref(),
                Some("protected request material was not forwarded")
            );
        }
    }
}
