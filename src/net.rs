//! Outbound network policy and DNS.
//!
//! The client decides where to connect, and it is ArenaNet's code running in a
//! sandboxed JS realm — so the host, not the client, decides where that is
//! allowed to go. Without this a compromised or malicious client build could
//! use the app as a pivot into whatever is reachable from the user's machine.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 6112 is the Guild Wars game/login port; 80 and 443 cover the file and web
/// services. Nothing else is a legitimate destination for this client.
const ALLOWED_PORTS: [u16; 3] = [6112, 80, 443];

#[derive(Debug)]
pub enum NetError {
    BadDestination(String),
    PortNotAllowed(u16),
    NotPublicUnicast(IpAddr),
    Resolve(String),
    Connect(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadDestination(d) => write!(f, "malformed destination: {d}"),
            Self::PortNotAllowed(p) => write!(f, "port {p} is not allowed"),
            Self::NotPublicUnicast(ip) => write!(f, "{ip} is not a public unicast address"),
            Self::Resolve(m) => write!(f, "resolve failed: {m}"),
            Self::Connect(m) => write!(f, "connect failed: {m}"),
        }
    }
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
                // 240.0.0.0/4 reserved.
                || v4.octets()[0] >= 240)
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
pub fn resolve(host: &str) -> Result<Ipv4Addr, NetError> {
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return check_public(IpAddr::V4(v4)).map(|_| v4);
    }
    let addrs = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| NetError::Resolve(e.to_string()))?;
    for addr in addrs {
        if let SocketAddr::V4(v4) = addr {
            let ip = *v4.ip();
            if is_public_unicast(IpAddr::V4(ip)) {
                return Ok(ip);
            }
        }
    }
    Err(NetError::Resolve(format!("no public A record for {host}")))
}

fn check_public(ip: IpAddr) -> Result<(), NetError> {
    is_public_unicast(ip)
        .then_some(())
        .ok_or(NetError::NotPublicUnicast(ip))
}

/// Dial `destination`, enforcing the port allowlist and re-checking the
/// resolved address. The check has to happen on the address actually connected
/// to, not the name — a name can resolve to anything.
pub fn connect(destination: &str) -> Result<TcpStream, NetError> {
    let (host, port) = parse_destination(destination)?;
    let candidates: Vec<SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| NetError::Resolve(e.to_string()))?
        .filter(|addr| is_public_unicast(addr.ip()))
        .collect();

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
}
