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

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

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
const REQUEST_DROP: [&str; 8] = [
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "origin",
    "referer",
    "transfer-encoding",
    "x-gwnative-token",
];

/// Upstream's transport framing describes its connection, not ours, and its
/// content policy describes its origin, not the page's.
const RESPONSE_DROP: [&str; 7] = [
    "connection",
    "content-encoding",
    "content-length",
    "content-security-policy",
    "content-security-policy-report-only",
    "transfer-encoding",
    "x-content-type-options",
];

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The host a route label stands for, or `None` if it is not one of the five.
pub fn host(route: &str) -> Option<&'static str> {
    ROUTES
        .iter()
        .find(|(name, _)| *name == route)
        .map(|(_, host)| *host)
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // Followed by hand below, so a redirect that leaves the allowlisted
            // host is refused instead of quietly fetched.
            .max_redirects(0)
            // A 401 is an answer the client knows how to render; it is not a
            // transport failure and must reach the page intact.
            .http_status_as_error(false)
            .build()
            .new_agent()
    })
}

/// Forward one request. `tail` is the path after the route label, leading slash
/// included; `query` is the raw query string without its `?`.
pub fn forward(
    route: &str,
    tail: &str,
    query: &str,
    method: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Result<Reply, String> {
    let host = host(route).ok_or_else(|| format!("unknown proxy route: {route}"))?;
    let url = if query.is_empty() {
        format!("https://{host}{tail}")
    } else {
        format!("https://{host}{tail}?{query}")
    };

    let forwarded: Vec<(&str, &str)> = headers
        .iter()
        .filter(|(name, _)| !REQUEST_DROP.contains(&name.as_str()))
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    // The bodies here are already compressed or small, and decoding one
    // transfer layer only to re-frame it below buys nothing.
    let mut response = match method {
        "GET" => {
            let mut request = agent().get(&url).header("accept-encoding", "identity");
            for (name, value) in forwarded {
                request = request.header(name, value);
            }
            request.call()
        }
        "POST" | "PUT" => {
            let mut request = match method {
                "POST" => agent().post(&url),
                _ => agent().put(&url),
            }
            .header("accept-encoding", "identity");
            for (name, value) in forwarded {
                request = request.header(name, value);
            }
            request.send(&body)
        }
        _ => return Err(format!("method not allowed: {method}")),
    }
    .map_err(|e| e.to_string())?;

    let status = response.status().as_u16();
    let mut out = Vec::new();
    for (name, value) in response.headers() {
        let name = name.as_str().to_ascii_lowercase();
        if RESPONSE_DROP.contains(&name.as_str()) {
            continue;
        }
        let Ok(value) = value.to_str() else { continue };
        if name == "location" {
            match rewrite_location(route, host, value) {
                Some(safe) => out.push((name, safe)),
                // Dropping the header turns the redirect into a bare 3xx, which
                // the client reports as a failed request rather than following
                // somewhere this host never vouched for.
                None => eprintln!("[proxy] {route}: refused a redirect to {value}"),
            }
            continue;
        }
        out.push((name, value.to_owned()));
    }

    let mut buffer = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut buffer)
        .map_err(|e| e.to_string())?;
    if buffer.len() > MAX_BODY {
        return Err(format!("response body over {MAX_BODY} bytes"));
    }

    Ok(Reply {
        status,
        headers: out,
        body: buffer,
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
}
