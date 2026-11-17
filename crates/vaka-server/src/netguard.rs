//! Outbound request guard for user-supplied URLs (webhook and Slack alert targets).
//!
//! Alert targets are chosen by members and write tokens, so without a guard they could make
//! Vaka call internal services (ClickHouse, cloud metadata, other apps on a private network).
//! Every address a target resolves to must be public, IP-literal hosts are checked directly,
//! and redirects are never followed. `VAKA_ALLOW_PRIVATE_ALERT_TARGETS=true` turns the address
//! check off for setups that really alert to an internal host.

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// Whether an address is globally routable (not loopback, private, link-local, CGNAT,
/// unique-local, multicast, documentation, benchmarking or otherwise reserved).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking
        || o[0] >= 240) // reserved
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local (incl. Fly 6PN fdaa::/16)
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64 can reach IPv4-private space
        || s[0] == 0x2002) // 6to4 embeds arbitrary IPv4
}

/// Check a target URL before it is stored or called: http(s) only, a host, and (unless private
/// targets are allowed) no IP literal outside public space. Hostnames are checked at connect time
/// by [`PublicResolver`], so DNS rebinding can't slip past.
pub fn check_url(raw: &str, allow_private: bool) -> Result<url::Url, String> {
    let u = url::Url::parse(raw.trim()).map_err(|e| format!("not a valid URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("URL must use http or https".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("URL must not contain credentials".into());
    }
    let host = u.host().ok_or("URL needs a host")?;
    if !allow_private {
        let literal = match host {
            url::Host::Ipv4(a) => Some(IpAddr::V4(a)),
            url::Host::Ipv6(a) => Some(IpAddr::V6(a)),
            url::Host::Domain(d) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                if d == "localhost"
                    || d.ends_with(".localhost")
                    || d.ends_with(".internal")
                    || d.ends_with(".local")
                {
                    return Err(format!("`{d}` is a private host; alert targets must be public (set VAKA_ALLOW_PRIVATE_ALERT_TARGETS=true to allow)"));
                }
                None
            }
        };
        if let Some(ip) = literal.filter(|ip| !is_public_ip(*ip)) {
            return Err(format!("{ip} is not a public address; alert targets must be public (set VAKA_ALLOW_PRIVATE_ALERT_TARGETS=true to allow)"));
        }
    }
    Ok(u)
}

/// DNS resolver that drops non-public addresses and fails when none are left.
pub struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> =
                addrs.into_iter().filter(|a| is_public_ip(a.ip())).collect();
            if public.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> =
                    format!("{host} does not resolve to a public address").into();
                return Err(err);
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// HTTP client for alert deliveries: no redirects, public addresses only (unless allowed).
pub fn alert_client(allow_private: bool) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Vaka-Webhook/1");
    if !allow_private {
        b = b.dns_resolver(Arc::new(PublicResolver));
    }
    Ok(b.build()?)
}

// ---------------------------------------------------------------------------
// Monitor targets
// ---------------------------------------------------------------------------

/// An address range from `VAKA_MONITOR_ALLOW_CIDRS`, like `10.0.0.0/8` or `fd00::/8`. A bare
/// address is a single host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cidr {
    net: IpAddr,
    bits: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Result<Cidr, String> {
        let s = s.trim();
        let (a, b) = s.split_once('/').unwrap_or((s, ""));
        let net: IpAddr = a.parse().map_err(|_| format!("`{s}` is not an IP range like 10.0.0.0/8"))?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let bits = if b.is_empty() { max } else { b.parse::<u8>().ok().filter(|n| *n <= max).ok_or_else(|| format!("`{s}` has a bad prefix length"))? };
        Ok(Cidr { net, bits })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) if self.net.is_ipv4() => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => return false,
            },
            ip => ip,
        };
        match (self.net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = if self.bits == 0 { 0 } else { u32::MAX << (32 - self.bits as u32) };
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = if self.bits == 0 { 0 } else { u128::MAX << (128 - self.bits as u32) };
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

/// Parse a comma- or space-separated list of ranges.
pub fn parse_cidrs(s: &str) -> Result<Vec<Cidr>, String> {
    s.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()).map(Cidr::parse).collect()
}

/// Which addresses monitor checks may reach: public ones, plus the operator's allowlist.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    pub allow: Vec<Cidr>,
}

impl Guard {
    pub fn from_env() -> Guard {
        let raw = std::env::var("VAKA_MONITOR_ALLOW_CIDRS").unwrap_or_default();
        match parse_cidrs(&raw) {
            Ok(allow) => Guard { allow },
            Err(e) => {
                tracing::warn!("ignoring VAKA_MONITOR_ALLOW_CIDRS: {e}");
                Guard::default()
            }
        }
    }

    pub fn permits(&self, ip: IpAddr) -> bool {
        is_public_ip(ip) || self.allow.iter().any(|c| c.contains(ip))
    }

    /// Check a monitor URL before it is stored or called. Hostnames are checked again at
    /// connect time by [`GuardResolver`].
    pub fn check_url(&self, raw: &str) -> Result<url::Url, String> {
        const HINT: &str = "monitors can only reach public addresses unless the operator allows a range in VAKA_MONITOR_ALLOW_CIDRS";
        let u = url::Url::parse(raw.trim()).map_err(|e| format!("not a valid URL: {e}"))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err("URL must use http or https".into());
        }
        if !u.username().is_empty() || u.password().is_some() {
            return Err("URL must not contain credentials; use secret headers".into());
        }
        match u.host().ok_or("URL needs a host")? {
            url::Host::Ipv4(a) if !self.permits(IpAddr::V4(a)) => Err(format!("{a} is not a public address; {HINT}")),
            url::Host::Ipv6(a) if !self.permits(IpAddr::V6(a)) => Err(format!("{a} is not a public address; {HINT}")),
            url::Host::Domain(d) if self.allow.is_empty() => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                if d == "localhost" || d.ends_with(".localhost") || d.ends_with(".internal") || d.ends_with(".local") {
                    return Err(format!("`{d}` is a private host; {HINT}"));
                }
                Ok(u)
            }
            _ => Ok(u),
        }
    }
}

/// DNS resolver for monitor checks: keeps the addresses [`Guard::permits`].
pub struct GuardResolver(pub Arc<Guard>);

impl Resolve for GuardResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let guard = self.0.clone();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let ok: Vec<SocketAddr> = addrs.into_iter().filter(|a| guard.permits(a.ip())).collect();
            if ok.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> =
                    format!("{host} does not resolve to a public address (VAKA_MONITOR_ALLOW_CIDRS can allow private ranges)").into();
                return Err(err);
            }
            Ok(Box::new(ok.into_iter()) as Addrs)
        })
    }
}

/// HTTP client for monitor checks. Redirects are followed (up to 5) only to URLs the guard
/// allows, every connection resolves through [`GuardResolver`], and the peer certificate is
/// kept for TLS expiry. `follow` false never follows redirects.
pub fn monitor_client(guard: Arc<Guard>, follow: bool) -> anyhow::Result<reqwest::Client> {
    let g = guard.clone();
    let policy = if follow {
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("more than 5 redirects");
            }
            let to = attempt.url().to_string();
            match g.check_url(&to) {
                Ok(_) => attempt.follow(),
                Err(e) => attempt.error(format!("redirect to {to} refused: {e}")),
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    Ok(reqwest::Client::builder()
        .redirect(policy)
        .dns_resolver(Arc::new(GuardResolver(guard)))
        .tls_info(true)
        .pool_max_idle_per_host(0)
        .user_agent("Vaka-Monitor/1")
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fdaa::3",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} should be private");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} should be public");
        }
    }

    #[test]
    fn checks_urls() {
        assert!(check_url("https://example.com/hook", false).is_ok());
        assert!(check_url("http://127.0.0.1:8123/?query=x", false).is_err());
        assert!(check_url("http://[::1]/", false).is_err());
        assert!(check_url("http://169.254.169.254/latest", false).is_err());
        assert!(check_url("http://localhost:8080", false).is_err());
        assert!(check_url("http://vaka-clickhouse.internal:8123", false).is_err());
        assert!(check_url("http://user:pw@example.com", false).is_err());
        assert!(check_url("ftp://example.com", false).is_err());
        assert!(check_url("http://127.0.0.1:9/x", true).is_ok());
    }

    #[tokio::test]
    async fn resolver_refuses_names_that_point_inside() {
        let r = PublicResolver.resolve("localhost".parse().unwrap()).await;
        assert!(r.is_err(), "localhost must not resolve for alert deliveries");
    }

    #[tokio::test]
    async fn client_refuses_private_hosts_at_connect_time() {
        let c = alert_client(false).unwrap();
        let e = c.get("http://localhost:9/").send().await.unwrap_err();
        assert!(format!("{e:?}").contains("public address"), "{e:?}");
    }

    #[test]
    fn cidr_allowlist() {
        let c = parse_cidrs("10.0.0.0/8, 192.168.1.5 fd00::/8").unwrap();
        assert_eq!(c.len(), 3);
        let g = Guard { allow: c };
        assert!(g.permits("10.2.3.4".parse().unwrap()));
        assert!(g.permits("192.168.1.5".parse().unwrap()));
        assert!(!g.permits("192.168.1.6".parse().unwrap()));
        assert!(g.permits("fd12::1".parse().unwrap()));
        assert!(g.permits("::ffff:10.0.0.1".parse().unwrap()));
        assert!(!g.permits("127.0.0.1".parse().unwrap()));
        assert!(g.permits("8.8.8.8".parse().unwrap()));
        assert!(parse_cidrs("10.0.0.0/33").is_err());
        assert!(parse_cidrs("nope").is_err());
        assert!(g.check_url("http://10.1.1.1:8080/health").is_ok());
        assert!(g.check_url("http://127.0.0.1/").is_err());
        assert!(Guard::default().check_url("http://localhost/").is_err());
        assert!(Guard::default().check_url("https://user:pw@example.com/").is_err());
        assert!(Guard::default().check_url("ftp://example.com/").is_err());
        assert!(Guard::default().check_url("https://example.com/").is_ok());
    }
}
