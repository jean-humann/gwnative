//! Outbound network policy and DNS.
//!
//! The client decides where to connect, and it is ArenaNet's code running in a
//! sandboxed JS realm — so the host, not the client, decides where that is
//! allowed to go. Without this a compromised or malicious client build could
//! use the app as a pivot into whatever is reachable from the user's machine.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 6112 is the Guild Wars game/login port; 80 and 443 cover the file and web
/// services. Nothing else is a legitimate destination for this client.
const ALLOWED_PORTS: [u16; 3] = [6112, 80, 443];

/// The only zones this client has any business asking about. Game
/// infrastructure lives under the first; the web services under the second.
const ALLOWED_DOMAINS: [&str; 2] = ["arenanetworks.com", "guildwars.com"];

/// Where to ask when the system resolver will not answer. Used only after
/// `/etc/resolv.conf` has been tried and only for names already inside the
/// allowlist above.
const FALLBACK_RESOLVERS: [Ipv4Addr; 2] = [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];

const DNS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub enum NetError {
    BadDestination(String),
    PortNotAllowed(u16),
    NameNotAllowed(String),
    NotPublicUnicast(IpAddr),
    Resolve(String),
    Connect(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadDestination(d) => write!(f, "malformed destination: {d}"),
            Self::PortNotAllowed(p) => write!(f, "port {p} is not allowed"),
            Self::NameNotAllowed(n) => write!(f, "{n} is not an ArenaNet name"),
            Self::NotPublicUnicast(ip) => write!(f, "{ip} is not a public unicast address"),
            Self::Resolve(m) => write!(f, "resolve failed: {m}"),
            Self::Connect(m) => write!(f, "connect failed: {m}"),
        }
    }
}

/// The one spelling of a name that everything downstream compares against.
///
/// DNS is case-insensitive and a trailing dot is the root, so `File1.ArenaNetworks.Com.`
/// and `file1.arenanetworks.com` are the same name — but not to a suffix check,
/// which is exactly the difference an allowlist bypass is made of.
pub fn normalize(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Whether a normalised name is inside one of the allowed zones.
///
/// A suffix match, and deliberately not a substring or `ends_with` on the bare
/// domain: `arenanetworks.com.evil.com` ends with neither `.arenanetworks.com`
/// nor the domain itself, and `notarenanetworks.com` is refused because the
/// boundary dot is required.
pub fn allowed_name(host: &str) -> bool {
    !host.is_empty()
        && ALLOWED_DOMAINS
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Split `host:port`, accepting the bracketed form for IPv6 literals.
pub fn parse_destination(destination: &str) -> Result<(String, u16), NetError> {
    let bad = || NetError::BadDestination(destination.to_owned());
    let (host, port) = match destination.strip_prefix('[') {
        Some(rest) => {
            let (host, tail) = rest.split_once(']').ok_or_else(bad)?;
            (host.to_owned(), tail.strip_prefix(':').ok_or_else(bad)?)
        }
        None => {
            let (host, port) = destination.rsplit_once(':').ok_or_else(bad)?;
            (host.to_owned(), port)
        }
    };
    let port: u16 = port.parse().map_err(|_| bad())?;
    if host.is_empty() {
        return Err(bad());
    }
    if !ALLOWED_PORTS.contains(&port) {
        return Err(NetError::PortNotAllowed(port));
    }
    Ok((host, port))
}

/// Reject anything that is not routable on the public internet. This is what
/// keeps a connect from reaching the loopback host, the local network, link
/// local metadata services, or a multicast group.
pub fn is_public_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_documentation()
                // 100.64.0.0/10 carrier-grade NAT and 198.18.0.0/15 benchmarking.
                || matches!(v4.octets(), [100, b, ..] if (64..128).contains(&b))
                || matches!(v4.octets(), [198, 18 | 19, ..])
                // 240.0.0.0/4 reserved, and 0.0.0.0/8 "this network" — the
                // whole block, not just the unspecified address. `geodc`
                // answers with `0.0.1.2` to name a datacenter rather than a
                // host, and that answer must reach the client without ever
                // being dialable.
                || v4.octets()[0] >= 240
                || v4.octets()[0] == 0)
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // Unique local fc00::/7 and link local fe80::/10.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped addresses would otherwise bypass the v4 rules.
                || v6.to_ipv4_mapped().is_some())
        }
    }
}

/// Resolve `host` to a single IPv4 address, which is the shape the client's
/// `dns.resolve` expects back.
///
/// Deliberately does *not* require the answer to be routable. `geodc` names a
/// datacenter by answering `0.0.1.2`, which the client reads as an identifier
/// and never dials; filtering it here would break datacenter selection while
/// protecting nothing, because the address that gets dialed is checked at
/// [`connect`] — on the address, where a name cannot lie about it.
///
/// The name is checked, though. It is the only place a name *can* be checked:
/// after this returns, the client has a bare address and no memory of what it
/// asked for.
pub fn resolve(name: &str) -> Result<Ipv4Addr, NetError> {
    let host = normalize(name);
    if host.is_empty() {
        return Err(NetError::BadDestination(name.to_owned()));
    }
    // An IP literal is not a name, and answering one would let the client
    // launder any address it likes through a route that looks like DNS.
    if host.parse::<IpAddr>().is_ok() || !allowed_name(&host) {
        return Err(NetError::NameNotAllowed(host));
    }

    let mut tried: Vec<String> = Vec::new();
    match (host.as_str(), 0u16).to_socket_addrs() {
        Ok(addrs) => {
            if let Some(ip) = addrs.into_iter().find_map(|addr| match addr {
                SocketAddr::V4(v4) => Some(*v4.ip()),
                SocketAddr::V6(_) => None,
            }) {
                return Ok(ip);
            }
            tried.push("getaddrinfo: no A record".to_owned());
        }
        Err(e) => tried.push(format!("getaddrinfo: {e}")),
    }

    // The system resolver refusing an answer is not the same as there being no
    // answer: `0.0.1.2` is a well-formed A record that getaddrinfo, and some
    // upstream resolvers, decline to hand back. Asking on the wire is the only
    // way to see what the zone actually said.
    for server in resolvers() {
        match raw_query(&host, server) {
            Ok(ip) => return Ok(ip),
            Err(e) => tried.push(format!("{server}: {e}")),
        }
    }
    Err(NetError::Resolve(tried.join("; ")))
}

/// Nameservers to try on the wire: the system's first, then two public ones.
fn resolvers() -> Vec<Ipv4Addr> {
    let mut servers: Vec<Ipv4Addr> = std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.trim().parse::<Ipv4Addr>().ok())
        .collect();
    for fallback in FALLBACK_RESOLVERS {
        if !servers.contains(&fallback) {
            servers.push(fallback);
        }
    }
    servers
}

/// One A-record question, asked directly.
fn raw_query(host: &str, server: Ipv4Addr) -> Result<Ipv4Addr, NetError> {
    let dns = |e: std::io::Error| NetError::Resolve(e.to_string());
    let id = query_id();
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(dns)?;
    socket.set_read_timeout(Some(DNS_TIMEOUT)).map_err(dns)?;
    // Connected rather than sent-to: the kernel then drops replies from anyone
    // but this server, which is most of what an unconnected socket would have
    // to check by hand.
    socket.connect((server, 53)).map_err(dns)?;
    socket.send(&encode_query(id, host)?).map_err(dns)?;

    let mut buf = [0u8; 512];
    let read = socket.recv(&mut buf).map_err(dns)?;
    decode_answer(id, &buf[..read])
}

/// Something that differs between queries. Not a security property — the socket
/// is connected, so an answer has to come from the server that was asked — but
/// a fixed id would let a stale reply satisfy the next question.
fn query_id() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    (nanos as u16) ^ (std::process::id() as u16)
}

fn encode_query(id: u16, host: &str) -> Result<Vec<u8>, NetError> {
    let mut out = Vec::with_capacity(host.len() + 18);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // standard query, recursion desired
    out.extend_from_slice(&1u16.to_be_bytes()); // one question
    out.extend_from_slice(&[0; 6]); // no answer, authority or additional records
    for label in host.split('.') {
        let len = u8::try_from(label.len())
            .ok()
            .filter(|len| (1..=63).contains(len))
            .ok_or_else(|| NetError::BadDestination(host.to_owned()))?;
        out.push(len);
        out.extend_from_slice(label.as_bytes());
    }
    out.extend_from_slice(&[0, 0, 1, 0, 1]); // root label, QTYPE A, QCLASS IN
    Ok(out)
}

/// Walk past a name, whether it is spelled out or a compression pointer.
fn skip_name(data: &[u8], mut at: usize) -> Result<usize, NetError> {
    let truncated = || NetError::Resolve("truncated DNS name".to_owned());
    while at < data.len() {
        let len = data[at];
        if len == 0 {
            return Ok(at + 1);
        }
        // A pointer ends the name and is two bytes wide; it is not followed.
        // Nothing here needs the name it points at, and following one is how a
        // parser gets walked in a circle by a hostile answer.
        if len & 0xc0 == 0xc0 {
            return Ok(at + 2);
        }
        at += usize::from(len) + 1;
    }
    Err(truncated())
}

fn decode_answer(id: u16, data: &[u8]) -> Result<Ipv4Addr, NetError> {
    let bad = |m: &str| NetError::Resolve(m.to_owned());
    let be16 = |at: usize| -> Result<u16, NetError> {
        data.get(at..at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .ok_or_else(|| bad("truncated DNS reply"))
    };
    if data.len() < 12 || be16(0)? != id {
        return Err(bad("DNS reply does not answer this question"));
    }
    let rcode = be16(2)? & 0xf;
    if rcode != 0 {
        return Err(NetError::Resolve(format!("DNS rcode {rcode}")));
    }
    let (questions, answers) = (be16(4)?, be16(6)?);

    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(data, at)? + 4; // QTYPE and QCLASS
    }
    for _ in 0..answers {
        at = skip_name(data, at)?;
        let (rtype, rdlen) = (be16(at)?, be16(at + 8)?);
        at += 10;
        // Type A with four bytes of data, and nothing else: a CNAME or an AAAA
        // in the same answer is skipped rather than misread.
        if rtype == 1 && rdlen == 4 {
            let quad = data
                .get(at..at + 4)
                .ok_or_else(|| bad("truncated DNS answer"))?;
            return Ok(Ipv4Addr::new(quad[0], quad[1], quad[2], quad[3]));
        }
        at += usize::from(rdlen);
    }
    Err(bad("no A record"))
}

/// Dial `destination`, enforcing the port allowlist and re-checking the
/// resolved address. The check has to happen on the address actually connected
/// to, not the name — a name can resolve to anything.
pub fn connect(destination: &str) -> Result<TcpStream, NetError> {
    let (host, port) = parse_destination(destination)?;
    // A destination is normally an address the client got from `resolve`, and
    // for those the address check below is the whole story. When it is a name
    // instead, that check comes too late to say anything about *which* host was
    // asked for: the port allowlist alone would leave every public machine on
    // the internet one string away.
    let host = normalize(&host);
    if host.parse::<IpAddr>().is_err() && !allowed_name(&host) {
        return Err(NetError::NameNotAllowed(host));
    }
    let (candidates, refused): (Vec<SocketAddr>, Vec<SocketAddr>) = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| NetError::Resolve(e.to_string()))?
        .partition(|addr| is_public_unicast(addr.ip()));
    // An allowed name whose only answer is unroutable is the interesting case —
    // a rebind, or the datacenter sentinel arriving somewhere it was not meant
    // to — and it deserves to be said as itself rather than as "connect failed".
    if candidates.is_empty()
        && let Some(addr) = refused.first()
    {
        return Err(NetError::NotPublicUnicast(addr.ip()));
    }

    let mut last = None;
    for addr in &candidates {
        match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                // Game traffic is latency-sensitive and small; Nagle would
                // trade responsiveness for coalescing nobody asked for.
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last = Some(e.to_string()),
        }
    }
    Err(match last {
        Some(detail) => NetError::Connect(detail),
        None => NetError::Resolve(format!("no public address for {host}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_game_destinations() {
        assert_eq!(
            parse_destination("File7.ArenaNetworks.com:6112").unwrap(),
            ("File7.ArenaNetworks.com".to_owned(), 6112)
        );
        assert_eq!(
            parse_destination("[2001:db8::1]:443").unwrap(),
            ("2001:db8::1".to_owned(), 443)
        );
    }

    #[test]
    fn refuses_other_ports() {
        assert!(matches!(
            parse_destination("example.com:22"),
            Err(NetError::PortNotAllowed(22))
        ));
        assert!(parse_destination("example.com").is_err());
        assert!(parse_destination(":6112").is_err());
    }

    #[test]
    fn refuses_private_and_local_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "240.0.0.1",
        ] {
            assert!(
                !is_public_unicast(ip.parse().unwrap()),
                "{ip} must be refused"
            );
        }
    }

    #[test]
    fn allows_public_addresses() {
        for ip in ["8.8.8.8", "64.25.34.1", "2606:4700::1111"] {
            assert!(
                is_public_unicast(ip.parse().unwrap()),
                "{ip} must be allowed"
            );
        }
    }

    #[test]
    fn ipv4_mapped_ipv6_cannot_smuggle_loopback() {
        assert!(!is_public_unicast("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_public_unicast("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn only_arenanet_names_are_answered_or_dialled() {
        for name in [
            "arenanetworks.com",
            "File1.ArenaNetworks.com",
            "arenanetworks.com.",
            "guildwars.com",
            "a.b.guildwars.com",
        ] {
            assert!(allowed_name(&normalize(name)), "{name} must be allowed");
        }
        // The three shapes of near-miss: a suffix that is really a prefix, a
        // label that merely ends in the domain, and nothing at all.
        for name in [
            "arenanetworks.com.evil.com",
            "notarenanetworks.com",
            "xguildwars.com",
            "evil.com",
            "",
        ] {
            assert!(!allowed_name(&normalize(name)), "{name:?} must be refused");
        }

        // The same rule reaches the dial, where a name would otherwise slip
        // past a check that only ever looks at addresses.
        assert!(matches!(
            connect("evil.com:443"),
            Err(NetError::NameNotAllowed(_))
        ));
        // And `resolve` refuses before it ever touches the network, so this
        // asserts policy rather than connectivity.
        assert!(matches!(
            resolve("evil.com"),
            Err(NetError::NameNotAllowed(_))
        ));
        assert!(
            matches!(resolve("8.8.8.8"), Err(NetError::NameNotAllowed(_))),
            "an address is not a name to resolve"
        );
    }

    #[test]
    fn the_datacenter_sentinel_is_answerable_but_not_dialable() {
        // 0.0.0.0/8 entire, not just the unspecified address: `geodc` answers
        // 0.0.1.2 to name a datacenter, and the client must receive it and must
        // never be able to open a socket to it.
        assert!(!is_public_unicast("0.0.1.2".parse().unwrap()));
        assert!(!is_public_unicast("0.1.2.3".parse().unwrap()));

        // Nothing in the resolve path filters it — proven by decoding an answer
        // that carries it, which is what the wire would hand back.
        let reply = a_record_reply(7, &[0, 0, 1, 2]);
        assert_eq!(decode_answer(7, &reply).unwrap(), Ipv4Addr::new(0, 0, 1, 2));
    }

    /// A minimal reply to `encode_query(id, "geodc.arenanetworks.com")`, with
    /// the answer's name given as a compression pointer, which is how a real
    /// resolver writes it.
    fn a_record_reply(id: u16, address: &[u8; 4]) -> Vec<u8> {
        let mut out = encode_query(id, "geodc.arenanetworks.com").unwrap();
        out[6..8].copy_from_slice(&1u16.to_be_bytes()); // one answer
        out.extend_from_slice(&[0xc0, 0x0c]); // pointer to the question's name
        out.extend_from_slice(&1u16.to_be_bytes()); // type A
        out.extend_from_slice(&1u16.to_be_bytes()); // class IN
        out.extend_from_slice(&60u32.to_be_bytes()); // ttl
        out.extend_from_slice(&4u16.to_be_bytes()); // rdlength
        out.extend_from_slice(address);
        out
    }

    #[test]
    fn a_query_is_asked_and_read_back_off_the_wire() {
        let query = encode_query(0x1234, "file1.arenanetworks.com").unwrap();
        assert_eq!(&query[..2], &[0x12, 0x34]);
        assert_eq!(&query[2..12], &[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&query[12..18], b"\x05file1");
        assert_eq!(&query[query.len() - 5..], &[0, 0, 1, 0, 1]);

        assert_eq!(
            decode_answer(0x1234, &a_record_reply(0x1234, &[64, 25, 34, 1])).unwrap(),
            Ipv4Addr::new(64, 25, 34, 1)
        );
    }

    #[test]
    fn a_reply_that_answers_something_else_is_not_an_answer() {
        let reply = a_record_reply(1, &[8, 8, 8, 8]);
        assert!(decode_answer(2, &reply).is_err(), "wrong query id");
        assert!(decode_answer(1, &reply[..8]).is_err(), "truncated header");
        assert!(
            decode_answer(1, &reply[..reply.len() - 2]).is_err(),
            "truncated rdata"
        );

        let mut refused = a_record_reply(1, &[8, 8, 8, 8]);
        refused[3] |= 5; // rcode REFUSED
        assert!(decode_answer(1, &refused).is_err());

        // A name that never terminates must end the walk rather than run off
        // the end of the buffer or loop.
        assert!(skip_name(&[3, b'a', b'b', b'c'], 0).is_err());
        assert_eq!(skip_name(&[0xc0, 0x0c, 9], 0).unwrap(), 2);
    }

    /// The fallback only runs when the system resolver declines, which on a
    /// healthy Mac it does not — `getaddrinfo` here hands back `0.0.1.2`
    /// happily. Left runnable rather than deleted, because the codec tests
    /// above cannot show that a real resolver accepts what `encode_query`
    /// writes: `cargo test -- --ignored the_wire`.
    #[test]
    #[ignore = "talks to a public resolver"]
    fn the_wire_path_asks_a_real_resolver() {
        for server in FALLBACK_RESOLVERS {
            assert_eq!(
                raw_query("geodc.arenanetworks.com", server).unwrap(),
                Ipv4Addr::new(0, 0, 1, 2),
                "{server} should have returned the datacenter sentinel"
            );
        }
    }

    #[test]
    fn the_resolver_list_always_has_somewhere_to_ask() {
        let servers = resolvers();
        for fallback in FALLBACK_RESOLVERS {
            assert_eq!(
                servers.iter().filter(|s| **s == fallback).count(),
                1,
                "{fallback} should appear exactly once"
            );
        }
    }
}
