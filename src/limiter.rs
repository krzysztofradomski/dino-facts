use axum::{
    extract::ConnectInfo,
    http::{HeaderName, Request},
};
use std::net::{IpAddr, SocketAddr};
use tower_governor::{key_extractor::KeyExtractor, GovernorError};

/// Rate-limit key: the client IP from a trusted proxy header when one is configured, else the peer address.
/// Only configure a header the proxy overwrites (e.g. `CF-Connecting-IP` behind Cloudflare); otherwise clients can forge it.
#[derive(Clone)]
pub struct ClientIp {
    header: Option<HeaderName>,
}

impl ClientIp {
    pub fn new(header: Option<&str>) -> Self {
        let header = header.and_then(|h| match HeaderName::from_bytes(h.trim().as_bytes()) {
            Ok(name) => Some(name),
            Err(_) => {
                eprintln!("ignoring invalid CLIENT_IP_HEADER {h:?}; using the peer address");
                None
            }
        });
        Self { header }
    }
}

impl KeyExtractor for ClientIp {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<IpAddr, GovernorError> {
        self.header
            .as_ref()
            .and_then(|h| req.headers().get(h))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse().ok())
            .or_else(|| req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()))
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(header: Option<(&str, &str)>, peer: Option<&str>) -> Request<()> {
        let mut builder = Request::builder();
        if let Some((k, v)) = header {
            builder = builder.header(k, v);
        }
        let mut r = builder.body(()).unwrap();
        if let Some(p) = peer {
            r.extensions_mut().insert(ConnectInfo(p.parse::<SocketAddr>().unwrap()));
        }
        r
    }

    fn key(e: &ClientIp, r: &Request<()>) -> Option<String> {
        e.extract(r).ok().map(|ip| ip.to_string())
    }

    #[test]
    fn uses_the_configured_header_over_the_peer() {
        let e = ClientIp::new(Some("CF-Connecting-IP"));
        let r = req(Some(("cf-connecting-ip", "203.0.113.7")), Some("10.0.0.2:1234"));
        assert_eq!(key(&e, &r), Some("203.0.113.7".into()));
    }

    #[test]
    fn falls_back_to_the_peer_when_the_header_is_missing_or_garbage() {
        let e = ClientIp::new(Some("cf-connecting-ip"));
        assert_eq!(key(&e, &req(None, Some("10.0.0.2:1234"))), Some("10.0.0.2".into()));
        let junk = req(Some(("cf-connecting-ip", "not-an-ip")), Some("10.0.0.2:1234"));
        assert_eq!(key(&e, &junk), Some("10.0.0.2".into()));
    }

    #[test]
    fn ignores_forwarding_headers_unless_configured() {
        let e = ClientIp::new(None);
        let r = req(Some(("cf-connecting-ip", "203.0.113.7")), Some("10.0.0.2:1234"));
        assert_eq!(key(&e, &r), Some("10.0.0.2".into()));
    }

    #[test]
    fn understands_ipv6_and_bad_header_names() {
        let e = ClientIp::new(Some("cf-connecting-ip"));
        let r = req(Some(("cf-connecting-ip", "2001:db8::1")), Some("10.0.0.2:1234"));
        assert_eq!(key(&e, &r), Some("2001:db8::1".into()));
        let bad = ClientIp::new(Some("not a header"));
        let r = req(Some(("cf-connecting-ip", "203.0.113.7")), Some("10.0.0.2:1234"));
        assert_eq!(key(&bad, &r), Some("10.0.0.2".into()));
    }

    #[test]
    fn no_header_and_no_peer_is_an_error() {
        assert!(ClientIp::new(None).extract(&req(None, None)).is_err());
    }
}
