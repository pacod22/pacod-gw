//! The client's address behind a reverse proxy (PID_TRUSTED_PROXIES):
//! `X-Forwarded-For` is believed only from a trusted address, and read from the right.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

/// The private address ranges, as Caddy's `private_ranges` names them.
const PRIVATE_RANGES: &[&str] =
    &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "fc00::/7", "::1/128"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    fn parse(s: &str) -> Result<Self, String> {
        let (ip, prefix) = match s.split_once('/') {
            Some((ip, p)) => (ip, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr =
            ip.parse().map_err(|_| format!("{s:?} is not an address, a CIDR range or private_ranges"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(|| format!("{s:?}: bad prefix length"))?,
        };
        Ok(Net { addr: addr.to_canonical(), prefix })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        let bits = |ip: IpAddr| -> u128 {
            match ip {
                IpAddr::V4(v) => u128::from(u32::from(v)) << 96,
                IpAddr::V6(v) => u128::from(v),
            }
        };
        if self.addr.is_ipv4() != ip.is_ipv4() {
            return false;
        }
        // Both families sit at the top of 128 bits, so a prefix counts from there.
        if self.prefix == 0 {
            return true;
        }
        let mask = u128::MAX << (128 - u32::from(self.prefix));
        bits(self.addr) & mask == bits(ip) & mask
    }
}

/// The proxies whose `X-Forwarded-For` is believed.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<Net>);

impl TrustedProxies {
    /// Addresses, CIDR ranges and the keyword `private_ranges`, separated by commas.
    pub fn parse(list: &str) -> Result<Self, String> {
        let mut nets = Vec::new();
        for e in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if e == "private_ranges" {
                nets.extend(PRIVATE_RANGES.iter().map(|r| Net::parse(r).expect("a valid range")));
            } else {
                nets.push(Net::parse(e)?);
            }
        }
        Ok(Self(nets))
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|n| n.contains(ip))
    }

    /// The client's address: the peer, or, when the peer is a trusted proxy, the first
    /// forwarded address from the right that is not itself a trusted proxy.
    pub fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let peer = peer.ip().to_canonical();
        if !self.trusts(peer) {
            return peer;
        }
        let hops: Vec<&str> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .collect();
        let mut client = peer;
        for hop in hops.iter().rev() {
            let Some(ip) = parse_hop(hop) else { return client };
            client = ip.to_canonical();
            if !self.trusts(client) {
                return client;
            }
        }
        client
    }
}

fn parse_hop(hop: &str) -> Option<IpAddr> {
    hop.parse::<IpAddr>()
        .ok()
        .or_else(|| hop.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| hop.strip_prefix('[').and_then(|h| h.strip_suffix(']')).and_then(|h| h.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(xff: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", xff.parse().unwrap());
        h
    }

    #[test]
    fn ranges() {
        let t = TrustedProxies::parse("10.0.0.0/8, 192.0.2.7, fd00::/8").unwrap();
        assert!(t.trusts("10.1.2.3".parse().unwrap()));
        assert!(!t.trusts("11.0.0.1".parse().unwrap()));
        assert!(t.trusts("192.0.2.7".parse().unwrap()) && !t.trusts("192.0.2.8".parse().unwrap()));
        assert!(t.trusts("fd12::1".parse().unwrap()) && !t.trusts("fe80::1".parse().unwrap()));
        assert!(TrustedProxies::parse("nonsense").is_err() && TrustedProxies::parse("10.0.0.0/33").is_err());
    }

    #[test]
    fn forwarded_addresses_count_only_behind_a_trusted_proxy() {
        let t = TrustedProxies::parse("private_ranges").unwrap();
        let caddy: SocketAddr = "172.18.0.5:443".parse().unwrap();
        let outsider: SocketAddr = "203.0.113.9:4711".parse().unwrap();
        assert_eq!(t.client_ip(caddy, &headers("198.51.100.1")), "198.51.100.1".parse::<IpAddr>().unwrap());
        // A forged header from outside changes nothing.
        assert_eq!(t.client_ip(outsider, &headers("198.51.100.1")), "203.0.113.9".parse::<IpAddr>().unwrap());
        // The rightmost untrusted hop wins over what the client wrote itself.
        assert_eq!(
            t.client_ip(caddy, &headers("1.2.3.4, 198.51.100.1, 10.0.0.2")),
            "198.51.100.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            TrustedProxies::default().client_ip(caddy, &headers("1.2.3.4")),
            "172.18.0.5".parse::<IpAddr>().unwrap()
        );
    }
}
