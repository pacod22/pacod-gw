//! The address of the client, also behind a load balancer.
//!
//! Behind Caddy or another load balancer every connection comes from the balancer, so the
//! peer address says nothing about the client. The balancer passes the client's address in
//! `X-Forwarded-For`; that header is believed only when the connection comes from an address
//! the installation trusts (`PGW_TRUSTED_PROXIES`), since anyone else can write it.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use pacod_gw_events::egress::NetList;

use super::Gw;

/// The private address ranges, as Caddy's `private_ranges` names them.
const PRIVATE_RANGES: &[&str] =
    &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "fc00::/7", "::1/128"];

/// The load balancers whose `X-Forwarded-For` is believed.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(NetList);

impl TrustedProxies {
    /// Parses addresses, CIDR ranges and the keyword `private_ranges`.
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        let mut list = NetList::default();
        for e in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
            if e == "private_ranges" {
                for r in PRIVATE_RANGES {
                    list.push(NetList::parse_entry(r).expect("a range").expect("a valid range"));
                }
                continue;
            }
            match NetList::parse_entry(e) {
                Some(net) => list.push(net?),
                None => return Err(format!("\"{e}\" is not an address, a CIDR range or private_ranges")),
            }
        }
        Ok(Self(list))
    }

    /// The client's address. When the connection comes from a trusted proxy, the forwarded
    /// addresses are read from the right, the closest hop first, and the first one that is not
    /// itself a trusted proxy is the client.
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = peer.to_canonical();
        if self.0.is_empty() || !self.0.contains(peer) {
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
            // What cannot be read cannot be trusted: stop at the last hop that could.
            let Some(ip) = parse_hop(hop) else { return client };
            client = ip.to_canonical();
            if !self.0.contains(client) {
                return client;
            }
        }
        client
    }
}

/// An address as proxies write it: `192.0.2.1`, `192.0.2.1:4711`, `2001:db8::1` or
/// `[2001:db8::1]:4711`.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Ok(addr) = hop.parse::<SocketAddr>() {
        return Some(addr.ip());
    }
    hop.strip_prefix('[').and_then(|h| h.strip_suffix(']')).and_then(|h| h.parse().ok())
}

/// The client's address as a handler argument.
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<Arc<Gw>> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, gw: &Arc<Gw>) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |ConnectInfo(addr)| addr.ip());
        Ok(Self(gw.boot.trusted_proxies.client_ip(peer, &parts.headers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn proxies(entries: &[&str]) -> TrustedProxies {
        TrustedProxies::parse(&entries.iter().map(|e| e.to_string()).collect::<Vec<_>>()).unwrap()
    }

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn an_untrusted_peer_is_the_client_whatever_it_claims() {
        let p = proxies(&["10.0.0.0/8"]);
        assert_eq!(p.client_ip(ip("203.0.113.9"), &xff(&["198.51.100.1"])), ip("203.0.113.9"));
        assert_eq!(TrustedProxies::default().client_ip(ip("10.0.0.2"), &xff(&["198.51.100.1"])), ip("10.0.0.2"));
    }

    #[test]
    fn a_trusted_peer_passes_on_the_client() {
        let p = proxies(&["private_ranges"]);
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["198.51.100.1"])), ip("198.51.100.1"));
        // A client can prepend anything; the hop the trusted proxy added is the one that counts.
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["1.2.3.4, 198.51.100.1"])), ip("198.51.100.1"));
        // Several trusted hops in a row are skipped.
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["198.51.100.1, 10.1.1.1", "192.168.0.5"])), ip("198.51.100.1"));
    }

    #[test]
    fn without_an_untrusted_hop_the_left_most_counts() {
        let p = proxies(&["private_ranges"]);
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["10.9.9.9, 10.1.1.1"])), ip("10.9.9.9"));
        assert_eq!(p.client_ip(ip("10.0.0.2"), &HeaderMap::new()), ip("10.0.0.2"));
    }

    #[test]
    fn unreadable_hops_stop_the_walk() {
        let p = proxies(&["private_ranges"]);
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["198.51.100.1, unknown"])), ip("10.0.0.2"));
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["garbage, 10.1.1.1"])), ip("10.1.1.1"));
    }

    #[test]
    fn ports_and_ipv6_forms_are_read() {
        let p = proxies(&["private_ranges"]);
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["198.51.100.1:4711"])), ip("198.51.100.1"));
        assert_eq!(p.client_ip(ip("::1"), &xff(&["[2001:db8::1]:4711"])), ip("2001:db8::1"));
        assert_eq!(p.client_ip(ip("::1"), &xff(&["2001:db8::2"])), ip("2001:db8::2"));
        // An IPv4 peer seen through an IPv6 socket is still matched against IPv4 ranges.
        assert_eq!(p.client_ip(ip("::ffff:10.0.0.2"), &xff(&["198.51.100.1"])), ip("198.51.100.1"));
        assert_eq!(p.client_ip(ip("10.0.0.2"), &xff(&["::ffff:198.51.100.1"])), ip("198.51.100.1"));
    }

    #[test]
    fn entries_are_checked() {
        assert!(TrustedProxies::parse(&["10.0.0.0/33".into()]).is_err());
        assert!(TrustedProxies::parse(&["caddy".into()]).is_err());
        assert!(TrustedProxies::parse(&["192.168.1.7".into(), " ".into(), "fd00::/8".into()]).is_ok());
    }
}
