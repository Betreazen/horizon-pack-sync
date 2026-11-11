//! Determines the client address behind a local reverse proxy (Caddy, nginx).

use std::net::{IpAddr, Ipv4Addr};

use axum::http::HeaderMap;

const FORWARDED_FOR: &str = "x-forwarded-for";

/// Trusts `X-Forwarded-For` only from a loopback peer, which is the local proxy.
/// The last value is the one the proxy itself observed.
pub fn resolve(headers: &HeaderMap, peer: Option<IpAddr>) -> IpAddr {
    let Some(peer) = peer else {
        return IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    };
    if !peer.to_canonical().is_loopback() {
        return peer;
    }
    headers
        .get(FORWARDED_FOR)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const PEER: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const REMOTE: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));

    fn headers(value: &'static str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(FORWARDED_FOR, HeaderValue::from_static(value));
        map
    }

    #[test]
    fn uses_last_forwarded_value() {
        let ip = resolve(&headers("1.1.1.1, 2.2.2.2"), Some(PEER));
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2)));
    }

    #[test]
    fn falls_back_to_peer_without_header() {
        assert_eq!(resolve(&HeaderMap::new(), Some(PEER)), PEER);
    }

    #[test]
    fn ignores_garbage_header() {
        assert_eq!(resolve(&headers("not-an-ip"), Some(PEER)), PEER);
    }

    #[test]
    fn ignores_header_from_remote_peer() {
        assert_eq!(resolve(&headers("1.1.1.1"), Some(REMOTE)), REMOTE);
    }

    #[test]
    fn unknown_peer_is_unspecified() {
        assert_eq!(
            resolve(&headers("1.1.1.1"), None),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        );
    }
}
