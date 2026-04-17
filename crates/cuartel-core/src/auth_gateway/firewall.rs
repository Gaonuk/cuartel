//! Gateway-side firewall (spec task 5f).
//!
//! The gateway is an HTTPS reverse-proxy that agents inside the sandbox
//! dial as their single upstream. Left unchecked, a misconfigured rule —
//! or an operator deliberately proxying a dev upstream over loopback —
//! turns the gateway into an open proxy pointed at the host's private
//! network: the SQLite credential DB path, the keychain helper socket,
//! cloud metadata endpoints, the gateway's own admin port, etc.
//!
//! The firewall enforces an IP-literal allowlist by **class**: loopback,
//! unspecified, link-local, multicast, broadcast, RFC1918, ULA, carrier
//! NAT, and IPv4-mapped variants thereof are rejected unless the
//! operator explicitly flips `allow_private_upstreams` (only the proxy
//! tests and local development fixtures should do this).
//!
//! Hostnames that resolve to private addresses via DNS are intentionally
//! **not** checked here — that would require pinning DNS through a
//! custom resolver, which is a Phase-10 concern. The practical threat
//! 5f targets is a rule whose `upstream_authority` is a raw IP literal.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use serde::{Deserialize, Serialize};

/// Firewall knobs, embedded in `AuthGatewayConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallPolicy {
    /// When `true`, rules may point `upstream_authority` at loopback,
    /// link-local, private, or unspecified addresses. Default is `false`
    /// — production should never need this; tests and local fixtures
    /// flip it to exercise the proxy against `127.0.0.1`.
    #[serde(default)]
    pub allow_private_upstreams: bool,
}

impl Default for FirewallPolicy {
    fn default() -> Self {
        Self {
            allow_private_upstreams: false,
        }
    }
}

/// Reason emitted into an `AuditEvent::Blocked` when the firewall fires.
pub const BLOCK_REASON_PRIVATE_UPSTREAM: &str =
    "firewall: upstream authority resolves to a private/loopback address";

/// True iff `addr` falls into an address class the gateway should never
/// proxy to from an untrusted agent.
pub fn is_blocked_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_private() {
        return true;
    }
    if ip.is_link_local() || ip.is_broadcast() || ip.is_multicast() {
        return true;
    }
    let [a, b, _, _] = ip.octets();
    // RFC 1122 "this network" — 0.0.0.0/8. is_unspecified only catches 0.0.0.0.
    if a == 0 {
        return true;
    }
    // RFC 6598 carrier-grade NAT 100.64.0.0/10.
    if a == 100 && (b & 0xC0) == 64 {
        return true;
    }
    false
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segs = ip.segments();
    // Unique local fc00::/7.
    if (segs[0] & 0xFE00) == 0xFC00 {
        return true;
    }
    // Link-local fe80::/10.
    if (segs[0] & 0xFFC0) == 0xFE80 {
        return true;
    }
    // IPv4-mapped ::ffff:a.b.c.d — check the embedded v4 against v4 rules.
    if segs[0..5] == [0, 0, 0, 0, 0] && segs[5] == 0xFFFF {
        let v4 = Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            (segs[6] & 0xFF) as u8,
            (segs[7] >> 8) as u8,
            (segs[7] & 0xFF) as u8,
        );
        return is_blocked_v4(v4);
    }
    false
}

/// Parse a `host[:port]` authority into a `SocketAddr` **only when the
/// host portion is an IP literal**. DNS names return `None` — the caller
/// skips the firewall check for them (hostname-based rules terminate at
/// the HTTPS connector, which resolves + connects in one pass and is
/// outside 5f's scope).
pub fn parse_ip_authority(authority: &str) -> Option<SocketAddr> {
    if let Ok(sa) = authority.parse::<SocketAddr>() {
        return Some(sa);
    }
    // Bracketed IPv6 without port: "[::1]".
    if let Some(inner) = authority.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if let Ok(ip) = inner.parse::<IpAddr>() {
            return Some(SocketAddr::new(ip, 0));
        }
    }
    if let Ok(ip) = authority.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, 0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_v4_is_blocked() {
        assert!(is_blocked_ip("127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("127.255.255.254".parse().unwrap()));
    }

    #[test]
    fn loopback_v6_is_blocked() {
        assert!(is_blocked_ip("::1".parse().unwrap()));
    }

    #[test]
    fn unspecified_is_blocked() {
        assert!(is_blocked_ip("0.0.0.0".parse().unwrap()));
        assert!(is_blocked_ip("::".parse().unwrap()));
    }

    #[test]
    fn rfc1918_private_v4_is_blocked() {
        assert!(is_blocked_ip("10.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("172.16.0.1".parse().unwrap()));
        assert!(is_blocked_ip("172.31.255.254".parse().unwrap()));
        assert!(is_blocked_ip("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn link_local_is_blocked() {
        assert!(is_blocked_ip("169.254.169.254".parse().unwrap()));
        assert!(is_blocked_ip("fe80::1".parse().unwrap()));
    }

    #[test]
    fn ula_v6_is_blocked() {
        assert!(is_blocked_ip("fc00::1".parse().unwrap()));
        assert!(is_blocked_ip("fd12:3456:789a::1".parse().unwrap()));
    }

    #[test]
    fn carrier_grade_nat_is_blocked() {
        assert!(is_blocked_ip("100.64.0.1".parse().unwrap()));
        assert!(is_blocked_ip("100.127.255.254".parse().unwrap()));
    }

    #[test]
    fn broadcast_and_multicast_are_blocked() {
        assert!(is_blocked_ip("255.255.255.255".parse().unwrap()));
        assert!(is_blocked_ip("239.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("ff02::1".parse().unwrap()));
    }

    #[test]
    fn ipv4_mapped_v6_loopback_is_blocked() {
        assert!(is_blocked_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn public_v4_is_allowed() {
        assert!(!is_blocked_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_blocked_ip("1.1.1.1".parse().unwrap()));
        // 100.0.0.1 is NOT in the CGN block (100.64/10), so it's public.
        assert!(!is_blocked_ip("100.0.0.1".parse().unwrap()));
    }

    #[test]
    fn public_v6_is_allowed() {
        assert!(!is_blocked_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn parse_ip_authority_accepts_various_shapes() {
        assert_eq!(
            parse_ip_authority("127.0.0.1:8080").map(|s| s.ip().to_string()),
            Some("127.0.0.1".into())
        );
        assert_eq!(
            parse_ip_authority("127.0.0.1").map(|s| s.ip().to_string()),
            Some("127.0.0.1".into())
        );
        assert_eq!(
            parse_ip_authority("[::1]:8080").map(|s| s.ip().to_string()),
            Some("::1".into())
        );
        assert_eq!(
            parse_ip_authority("[::1]").map(|s| s.ip().to_string()),
            Some("::1".into())
        );
    }

    #[test]
    fn parse_ip_authority_returns_none_for_hostnames() {
        assert!(parse_ip_authority("api.anthropic.com").is_none());
        assert!(parse_ip_authority("api.anthropic.com:443").is_none());
    }

    #[test]
    fn firewall_policy_default_is_restrictive() {
        let p = FirewallPolicy::default();
        assert!(!p.allow_private_upstreams);
    }

    #[test]
    fn firewall_policy_roundtrips_through_json() {
        let p = FirewallPolicy {
            allow_private_upstreams: true,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: FirewallPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn firewall_policy_deserializes_from_empty_object() {
        // Old configs that predate 5f should deserialize cleanly with
        // the restrictive default.
        let p: FirewallPolicy = serde_json::from_str("{}").unwrap();
        assert!(!p.allow_private_upstreams);
    }
}
