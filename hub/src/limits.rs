//! A per-address request limit. A game start costs two requests and a server check one, so this
//! only stops abuse.

use std::net::{IpAddr, Ipv6Addr};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

const REQUESTS_PER_MINUTE: NonZeroU32 = NonZeroU32::new(60).expect("non-zero");
const IPV6_NETWORK_MASK: u128 = u128::MAX << 64;
/// How often the limiter forgets addresses whose quota has fully recovered.
const CLEANUP_EVERY: u64 = 1024;

#[derive(Debug)]
pub struct RequestLimiter {
    limiter: DefaultKeyedRateLimiter<IpAddr>,
    checks: AtomicU64,
}

impl Default for RequestLimiter {
    fn default() -> Self {
        Self {
            limiter: RateLimiter::keyed(Quota::per_minute(REQUESTS_PER_MINUTE)),
            checks: AtomicU64::new(0),
        }
    }
}

/// One subscriber usually controls a whole IPv6 /64, so the /64 is what gets limited.
fn limit_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(address) => IpAddr::V6(Ipv6Addr::from(u128::from(address) & IPV6_NETWORK_MASK)),
        v4 @ IpAddr::V4(_) => v4,
    }
}

impl RequestLimiter {
    /// Whether `ip` may make another request now.
    pub fn allow(&self, ip: IpAddr) -> bool {
        if self
            .checks
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(CLEANUP_EVERY)
        {
            self.limiter.retain_recent();
        }
        self.limiter.check_key(&limit_key(ip)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn the_sixty_first_request_in_a_minute_is_refused() {
        let limiter = RequestLimiter::default();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        for _ in 0..60 {
            assert!(limiter.allow(ip));
        }
        assert!(!limiter.allow(ip));
        assert!(limiter.allow(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))));
    }

    #[test]
    fn addresses_in_one_ipv6_network_share_a_limit() {
        let limiter = RequestLimiter::default();
        for _ in 0..60 {
            assert!(limiter.allow("2001:db8::1".parse().expect("address")));
        }
        assert!(!limiter.allow("2001:db8::ffff".parse().expect("address")));
    }
}
