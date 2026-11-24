//! IP-level backstop for the hostname fence.
//!
//! The fence decides by name: scope plus the loopback and lab lists. A name
//! the scope admitted can still answer DNS with an address no outbound
//! request may ever reach (an internal range, the cloud-metadata endpoint):
//! that is how DNS rebinding turns a scoped crawl into a trip around the
//! analyst's own network. [`IpGuard::block_reason`] re-checks the resolved
//! addresses before a load is allowed to reach the network.
//!
//! Callers exempt loopback and lab hosts by hostname before consulting this
//! module: reaching those is their whole purpose.
//!
//! `is_forbidden_ip` is ported from obscura-ssrf in the h4ckf0r0day/obscura
//! repository (Apache-2.0).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use url::Url;

use crate::fence::FenceReason;

/// How long one resolution stays trusted. Short on purpose: the verdict must
/// not outlive a DNS answer an attacker controls.
const TTL: Duration = Duration::from_secs(30);

/// Resolves a hostname to its candidate addresses. A trait so tests feed
/// answers without touching the network.
pub trait ResolveHosts: Send + Sync {
    fn resolve(&self, host: &str) -> Vec<IpAddr>;
}

/// The real resolver: a blocking lookup on the system stack.
struct SystemResolver;

impl ResolveHosts for SystemResolver {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        (host, 0)
            .to_socket_addrs()
            .map(|addrs| addrs.map(|addr| addr.ip()).collect())
            .unwrap_or_default()
    }
}

/// The guard proper: a resolver plus a short-TTL cache of its answers.
pub struct IpGuard {
    resolver: Arc<dyn ResolveHosts>,
    cache: Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>,
}

impl IpGuard {
    pub fn system() -> Self {
        Self::with_resolver(Arc::new(SystemResolver))
    }

    /// Tests install their own resolver here.
    pub fn with_resolver(resolver: Arc<dyn ResolveHosts>) -> Self {
        Self {
            resolver,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// `Some(reason)` when every address the host resolves to is forbidden:
    /// the load must not reach the network.
    ///
    /// A host with one public candidate is allowed (legitimate dual-stack
    /// setups mix ranges), and so is an empty resolution: an offline DNS
    /// hiccup must never silently block a crawl.
    pub fn block_reason(&self, url: &str) -> Option<FenceReason> {
        let parsed = Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        let host = parsed.host_str()?.to_owned();
        let addresses = self.resolved(&host);
        if addresses.is_empty() {
            return None;
        }
        addresses
            .iter()
            .all(|ip| is_forbidden_ip(*ip))
            .then_some(FenceReason::ForbiddenIp)
    }

    /// The cached resolution for `host`, refreshed when the TTL elapsed.
    fn resolved(&self, host: &str) -> Vec<IpAddr> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, addresses)) = cache.get(host)
            && at.elapsed() < TTL
        {
            return addresses.clone();
        }
        let addresses = self.resolver.resolve(host);
        cache.insert(host.to_owned(), (Instant::now(), addresses.clone()));
        addresses
    }
}

static GUARD: LazyLock<IpGuard> = LazyLock::new(IpGuard::system);

/// The shared guard the intercept path consults.
pub fn block_reason(url: &str) -> Option<FenceReason> {
    GUARD.block_reason(url)
}

pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                // std's is_private() covers only RFC1918, so add the IANA
                // special-purpose ranges that also host internal services and
                // are common SSRF targets:
                //   100.64.0.0/10  CGNAT / RFC6598 — cloud metadata (e.g.
                //                  Alibaba 100.100.100.200) lives here.
                //   198.18.0.0/15  benchmarking / RFC2544.
                //   192.88.99.0/24 6to4 relay anycast / RFC7526.
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                || (o[0] == 192 && o[1] == 88 && o[2] == 99)
                // Most of 192.0.0.0/24 is special-purpose and not globally
                // reachable. Keep the two globally reachable PCP anycast
                // assignments usable rather than blocking the entire /24.
                || (o[0] == 192
                    && o[1] == 0
                    && o[2] == 0
                    && o[3] != 9
                    && o[3] != 10)
                // 240.0.0.0/4 is reserved (255.255.255.255 was already
                // covered by is_broadcast()).
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
            {
                return true;
            }
            // Unwrap IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d)
            // forms and re-check the embedded v4 so e.g. [::ffff:127.0.0.1] or
            // [::ffff:169.254.169.254] cannot slip past the v6 arm.
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return is_forbidden_ip(IpAddr::V4(v4));
            }

            let s = v6.segments();
            // IPv4/IPv6 translation prefix (RFC 6052). Only /96 has a fixed
            // embedded-address position; the local-use /48 is therefore
            // blocked outright below.
            if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
                return is_forbidden_ip(IpAddr::V4(Ipv4Addr::new(
                    (s[6] >> 8) as u8,
                    s[6] as u8,
                    (s[7] >> 8) as u8,
                    s[7] as u8,
                )));
            }
            // 6to4 carries its IPv4 endpoint in bits 16..48.
            if s[0] == 0x2002 {
                return is_forbidden_ip(IpAddr::V4(Ipv4Addr::new(
                    (s[1] >> 8) as u8,
                    s[1] as u8,
                    (s[2] >> 8) as u8,
                    s[2] as u8,
                )));
            }
            // Teredo (RFC 4380, 2001:0::/32) carries the server's IPv4 in
            // bits 32..64 and the client's public IPv4, bit-inverted, in the
            // last 32 bits. Either one embedding a forbidden address is
            // refused, like the other IPv4-in-IPv6 forms above.
            if s[0] == 0x2001 && s[1] == 0 {
                let server =
                    Ipv4Addr::new((s[2] >> 8) as u8, s[2] as u8, (s[3] >> 8) as u8, s[3] as u8);
                let client = Ipv4Addr::new(
                    !(s[6] >> 8) as u8,
                    !s[6] as u8,
                    !(s[7] >> 8) as u8,
                    !s[7] as u8,
                );
                return is_forbidden_ip(IpAddr::V4(server)) || is_forbidden_ip(IpAddr::V4(client));
            }

            // Discard-only, local-use NAT64, and documentation prefixes.
            (s[0] == 0x100 && s[1] == 0 && s[2] == 0 && s[3] == 0)
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x3fff && s[1] & 0xf000 == 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    struct Fake(Vec<IpAddr>);

    impl ResolveHosts for Fake {
        fn resolve(&self, _host: &str) -> Vec<IpAddr> {
            self.0.clone()
        }
    }

    fn guard(ips: &[IpAddr]) -> IpGuard {
        IpGuard::with_resolver(Arc::new(Fake(ips.to_vec())))
    }

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn 사설_주소는_금지된다() {
        assert!(is_forbidden_ip(v4(10, 0, 0, 1)));
        assert!(is_forbidden_ip(v4(192, 168, 0, 1)));
        assert!(is_forbidden_ip(v4(172, 16, 0, 1)));
        assert!(is_forbidden_ip(v4(172, 31, 255, 255)));
    }

    #[test]
    fn 클라우드_메타데이터_주소는_금지된다() {
        assert!(is_forbidden_ip(v4(169, 254, 169, 254)));
    }

    #[test]
    fn cgnat와_벤치마크_대역도_금지된다() {
        // 클라우드 메타데이터가 이 대역에 사는 경우가 있다.
        assert!(is_forbidden_ip(v4(100, 100, 100, 200)));
        assert!(is_forbidden_ip(v4(100, 64, 0, 1)));
        assert!(is_forbidden_ip(v4(198, 18, 0, 1)));
    }

    #[test]
    fn ipv4를_감싼_ipv6도_잡는다() {
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().expect("valid v6");
        assert!(is_forbidden_ip(mapped));
        let metadata: IpAddr = "::ffff:169.254.169.254".parse().expect("valid v6");
        assert!(is_forbidden_ip(metadata));
    }

    #[test]
    fn 공개_주소는_허용된다() {
        assert!(!is_forbidden_ip(v4(93, 184, 216, 34)));
        assert!(!is_forbidden_ip(v4(1, 1, 1, 1)));
        let public: IpAddr = "2606:2800:220:1:248:1893:25c8:1946"
            .parse()
            .expect("valid v6");
        assert!(!is_forbidden_ip(public));
    }

    #[test]
    fn 전_주소가_금지면_차단한다() {
        let guard = guard(&[v4(10, 1, 2, 3), v4(192, 168, 1, 1)]);
        assert_eq!(
            guard.block_reason("http://rebound.test/admin"),
            Some(FenceReason::ForbiddenIp)
        );
    }

    #[test]
    fn 공개_후보가_하나라도_있으면_허용한다() {
        let guard = guard(&[v4(10, 1, 2, 3), v4(93, 184, 216, 34)]);
        assert_eq!(guard.block_reason("https://dualstack.test/page"), None);
    }

    #[test]
    fn 해석_실패는_차단하지_않는다() {
        let guard = guard(&[]);
        assert_eq!(guard.block_reason("http://offline.test/x"), None);
    }

    #[test]
    fn http_외_스킴은_검사하지_않는다() {
        let guard = guard(&[v4(10, 1, 2, 3)]);
        assert_eq!(guard.block_reason("file:///tmp/page.html"), None);
    }
}
