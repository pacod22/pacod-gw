//! Where the gateway may connect to (concept section 16, server-side request forgery).
//!
//! Provider base URLs are supplied by administrators. Without a check, a URL pointing at a
//! cloud metadata endpoint or an internal service would make the gateway fetch it with its own
//! network position. Public addresses are always allowed. Private, loopback and link-local
//! addresses are refused unless the installation explicitly allows the host or its range, as
//! is needed for a model server on the private network.
//!
//! The check runs twice: when a provider is saved (on the URL) and when a connection is opened
//! (on the resolved address), so a name that later resolves to an internal address is caught.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The installation's allow-list for non-public destinations.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    hosts: Vec<String>,
    nets: Vec<(IpAddr, u8)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressError {
    #[error("the URL is not valid: {0}")]
    BadUrl(String),
    #[error("only http and https URLs are allowed")]
    Scheme,
    #[error("the URL has no host")]
    NoHost,
    #[error(
        "{0} is a private, loopback or link-local address; add the host or its range to PGW_EGRESS_ALLOW_PRIVATE if this is intended"
    )]
    NotPublic(String),
}

impl EgressPolicy {
    /// Parses allow-list entries: host names, single addresses, or CIDR ranges.
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        let mut p = Self::default();
        for e in entries {
            let e = e.trim();
            if e.is_empty() {
                continue;
            }
            if let Some((addr, len)) = e.split_once('/') {
                let ip: IpAddr = addr.parse().map_err(|_| format!("\"{e}\" is not a CIDR range"))?;
                let len: u8 = len.parse().map_err(|_| format!("\"{e}\" is not a CIDR range"))?;
                let max = if ip.is_ipv4() { 32 } else { 128 };
                if len > max {
                    return Err(format!("\"{e}\": prefix length exceeds {max}"));
                }
                p.nets.push((ip, len));
            } else if let Ok(ip) = e.parse::<IpAddr>() {
                p.nets.push((ip, if ip.is_ipv4() { 32 } else { 128 }));
            } else {
                p.hosts.push(e.to_ascii_lowercase());
            }
        }
        Ok(p)
    }

    fn host_allowed(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.hosts.contains(&h)
    }

    fn net_allowed(&self, ip: IpAddr) -> bool {
        self.nets.iter().any(|(net, len)| in_net(ip, *net, *len))
    }

    /// Whether a connection to `ip`, reached through the name `host`, is allowed.
    pub fn allows(&self, host: &str, ip: IpAddr) -> bool {
        is_public(ip) || self.host_allowed(host) || self.net_allowed(ip)
    }

    /// Checks a provider URL when it is saved.
    pub fn check_url(&self, raw: &str) -> Result<url::Url, EgressError> {
        let u = url::Url::parse(raw).map_err(|e| EgressError::BadUrl(e.to_string()))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(EgressError::Scheme);
        }
        match u.host() {
            None => return Err(EgressError::NoHost),
            Some(url::Host::Ipv4(ip)) => self.check_literal(IpAddr::V4(ip))?,
            Some(url::Host::Ipv6(ip)) => self.check_literal(IpAddr::V6(ip))?,
            Some(url::Host::Domain(d)) => {
                if d.eq_ignore_ascii_case("localhost")
                    && !self.host_allowed(d)
                    && !self.net_allowed(Ipv4Addr::LOCALHOST.into())
                {
                    return Err(EgressError::NotPublic(d.to_owned()));
                }
            }
        }
        Ok(u)
    }

    fn check_literal(&self, ip: IpAddr) -> Result<(), EgressError> {
        if is_public(ip) || self.net_allowed(ip) || self.host_allowed(&ip.to_string()) {
            Ok(())
        } else {
            Err(EgressError::NotPublic(ip.to_string()))
        }
    }
}

fn in_net(ip: IpAddr, net: IpAddr, len: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - u32::from(len)) };
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let mask = if len == 0 { 0 } else { u128::MAX << (128 - u32::from(len)) };
            u128::from(a) & mask == u128::from(n) & mask
        }
        // An IPv4-mapped IPv6 address is matched against IPv4 ranges.
        (IpAddr::V6(a), IpAddr::V4(_)) => a.to_ipv4_mapped().is_some_and(|v4| in_net(IpAddr::V4(v4), net, len)),
        _ => false,
    }
}

/// True for addresses on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()          // 169.254.0.0/16, includes cloud metadata endpoints
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)   // 100.64.0.0/10 carrier-grade NAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18)   // 198.18.0.0/15 benchmarking
        || o[0] >= 240) // reserved
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00   // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80   // fe80::/10 link-local
        || (s[0] == 0x2001 && s[1] == 0x0db8)) // documentation
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(entries: &[&str]) -> EgressPolicy {
        EgressPolicy::parse(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn public_urls_are_allowed() {
        let p = policy(&[]);
        for ok in ["https://api.openai.com/v1", "https://api.anthropic.com", "http://8.8.8.8:8080/x"] {
            assert!(p.check_url(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn internal_targets_are_refused_by_default() {
        let p = policy(&[]);
        for bad in [
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://127.0.0.1:11434/v1",
            "http://localhost:11434/v1",
            "http://10.0.0.5/v1",
            "http://192.168.1.10/v1",
            "http://172.16.0.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:10.0.0.1]/", // IPv4-mapped private address
            "http://0.0.0.0/",
            "http://100.64.0.1/",
        ] {
            assert!(matches!(p.check_url(bad), Err(EgressError::NotPublic(_))), "{bad}");
        }
        assert_eq!(p.check_url("ftp://example.com"), Err(EgressError::Scheme));
        assert!(matches!(p.check_url("not a url"), Err(EgressError::BadUrl(_))));
    }

    #[test]
    fn allow_list_opens_specific_hosts_and_ranges() {
        let p = policy(&["ollama", "10.1.0.0/16", "127.0.0.1", "localhost"]);
        assert!(p.check_url("http://10.1.2.3:11434/v1").is_ok());
        assert!(p.check_url("http://127.0.0.1:9000").is_ok());
        assert!(p.check_url("http://localhost:9000").is_ok());
        assert!(matches!(p.check_url("http://10.2.0.1/"), Err(EgressError::NotPublic(_))));
        assert!(matches!(p.check_url("http://127.0.0.2/"), Err(EgressError::NotPublic(_))));
        // A named host is checked again when it is resolved.
        assert!(p.allows("ollama", "172.18.0.5".parse().unwrap()));
        assert!(!p.allows("other", "172.18.0.5".parse().unwrap()));
        assert!(p.allows("other", "10.1.9.9".parse().unwrap()));
        assert!(p.allows("api.openai.com", "104.18.6.192".parse().unwrap()));
    }

    #[test]
    fn a_public_name_resolving_to_an_internal_address_is_refused_at_connect_time() {
        let p = policy(&[]);
        assert!(p.check_url("https://rebind.example.com/v1").is_ok(), "the URL itself looks public");
        assert!(!p.allows("rebind.example.com", "169.254.169.254".parse().unwrap()));
        assert!(!p.allows("rebind.example.com", "127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn malformed_allow_list_entries_are_rejected() {
        assert!(EgressPolicy::parse(&["10.0.0.0/33".to_string()]).is_err());
        assert!(EgressPolicy::parse(&["x/8".to_string()]).is_err());
        assert!(EgressPolicy::parse(&[" ".to_string(), "".to_string()]).is_ok());
    }

    #[test]
    fn cidr_matching() {
        assert!(in_net("10.1.255.255".parse().unwrap(), "10.1.0.0".parse().unwrap(), 16));
        assert!(!in_net("10.2.0.0".parse().unwrap(), "10.1.0.0".parse().unwrap(), 16));
        assert!(in_net("1.2.3.4".parse().unwrap(), "0.0.0.0".parse().unwrap(), 0));
        assert!(in_net("fd00::5".parse().unwrap(), "fd00::".parse().unwrap(), 8));
    }
}
