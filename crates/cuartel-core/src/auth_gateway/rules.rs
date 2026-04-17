//! Rule matching and credential materialization for the auth gateway.
//!
//! Pure: no network, no filesystem, no credential store dependency in this
//! file. The proxy layer (PR-2) drives these types against a live
//! `CredentialStore`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use super::firewall::FirewallPolicy;

/// Dummy credential sentinel surfaced to VM-side agents. The real key never
/// leaves the host; the gateway swaps this value for the stored credential
/// after matching the request's `Host` header against a rule. Prefixed with
/// `sk-cuartel-` so audit logs trivially distinguish gateway-origin traffic.
pub const DUMMY_API_KEY: &str = "sk-cuartel-gateway";

/// What to do when an incoming request's host doesn't match any rule.
///
/// Default is `Reject` — matching the spec's security model ("VMs cannot
/// reach credential storage"). Passthrough exists solely for local
/// development / debugging where you want to see unmodified traffic flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissPolicy {
    Reject,
    Passthrough,
}

impl Default for MissPolicy {
    fn default() -> Self {
        Self::Reject
    }
}

/// A single hostname → credential mapping.
///
/// Matching is exact hostname for Phase 5c; glob/suffix support is deferred
/// until we have a real use case (e.g. Azure OpenAI custom domains).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthRule {
    /// Host header to match, e.g. `api.anthropic.com`.
    pub hostname: String,
    /// Credential store provider id, e.g. `anthropic`.
    pub provider_id: String,
    /// Credential store env key, e.g. `ANTHROPIC_API_KEY`.
    pub env_key: String,
    /// Header to set on the outgoing request, e.g. `x-api-key`.
    pub header_name: String,
    /// Template for the header value. `{key}` is replaced with the stored
    /// credential. Use `{key}` for raw (Anthropic) or `Bearer {key}` for
    /// OpenAI-style.
    pub header_format: String,
    /// Headers stripped from the incoming request before the injected auth
    /// header is added. Guards against a leaky agent forwarding the dummy
    /// key in a shape we didn't intend, or smuggling in a real key.
    #[serde(default = "default_strip_headers")]
    pub strip_headers: Vec<String>,
    /// Upstream URI scheme. Defaults to `https`. Plain `http` is supported
    /// for testing against fake upstreams and for the rare internal
    /// provider that speaks HTTP on a trusted network.
    #[serde(default = "default_scheme")]
    pub upstream_scheme: String,
    /// Optional upstream authority override (`host[:port]`). When `None`,
    /// the rule's `hostname` is used. Tests point this at `127.0.0.1:<port>`
    /// so traffic lands on a fake upstream while rule matching still keys
    /// off the public hostname the agent dialed.
    #[serde(default)]
    pub upstream_authority: Option<String>,
}

fn default_strip_headers() -> Vec<String> {
    vec!["authorization".to_string(), "x-api-key".to_string()]
}

fn default_scheme() -> String {
    "https".to_string()
}

impl AuthRule {
    /// Render the final header value by substituting `{key}` with `credential`.
    ///
    /// This is intentionally a plain string replace rather than a full
    /// templating engine — the format string is operator-controlled and the
    /// only substitution we ever need is `{key}`.
    pub fn render_header_value(&self, credential: &str) -> String {
        self.header_format.replace("{key}", credential)
    }
}

/// Gateway configuration: the rule set, the bind address, and what to do on
/// a rule miss. `bind` defaults to `127.0.0.1:0` (loopback, ephemeral port)
/// so the kernel picks a free port and the gateway never accidentally
/// listens on a routable interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthGatewayConfig {
    pub rules: Vec<AuthRule>,
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default)]
    pub on_miss: MissPolicy,
    /// Firewall policy for upstream authorities (task 5f). Defaults to
    /// rejecting loopback/private addresses; tests and local fixtures
    /// flip `allow_private_upstreams` to let the proxy talk to a fake
    /// upstream on 127.0.0.1.
    #[serde(default)]
    pub firewall: FirewallPolicy,
}

fn default_bind() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

impl AuthGatewayConfig {
    /// Seed configuration with the built-in rules and default bind/policy.
    pub fn with_default_rules() -> Self {
        Self {
            rules: default_rules(),
            bind: default_bind(),
            on_miss: MissPolicy::default(),
            firewall: FirewallPolicy::default(),
        }
    }

    /// Find the first rule matching `hostname` (case-insensitive). Returns
    /// `None` on a miss — callers apply `on_miss` policy themselves.
    pub fn match_host(&self, hostname: &str) -> Option<&AuthRule> {
        self.rules
            .iter()
            .find(|r| r.hostname.eq_ignore_ascii_case(hostname))
    }
}

impl Default for AuthGatewayConfig {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            bind: default_bind(),
            on_miss: MissPolicy::default(),
            firewall: FirewallPolicy::default(),
        }
    }
}

/// Built-in rules covering the providers cuartel's first-party harnesses
/// currently target. Extend this list as new harnesses land.
pub fn default_rules() -> Vec<AuthRule> {
    vec![
        AuthRule {
            hostname: "api.anthropic.com".to_string(),
            provider_id: "anthropic".to_string(),
            env_key: "ANTHROPIC_API_KEY".to_string(),
            header_name: "x-api-key".to_string(),
            header_format: "{key}".to_string(),
            strip_headers: default_strip_headers(),
            upstream_scheme: default_scheme(),
            upstream_authority: None,
        },
        AuthRule {
            hostname: "api.openai.com".to_string(),
            provider_id: "openai".to_string(),
            env_key: "OPENAI_API_KEY".to_string(),
            header_name: "Authorization".to_string(),
            header_format: "Bearer {key}".to_string(),
            strip_headers: default_strip_headers(),
            upstream_scheme: default_scheme(),
            upstream_authority: None,
        },
        AuthRule {
            hostname: "generativelanguage.googleapis.com".to_string(),
            provider_id: "google".to_string(),
            env_key: "GEMINI_API_KEY".to_string(),
            header_name: "x-goog-api-key".to_string(),
            header_format: "{key}".to_string(),
            strip_headers: default_strip_headers(),
            upstream_scheme: default_scheme(),
            upstream_authority: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rules_cover_first_party_providers() {
        let rules = default_rules();
        assert!(rules.iter().any(|r| r.provider_id == "anthropic"));
        assert!(rules.iter().any(|r| r.provider_id == "openai"));
        assert!(rules.iter().any(|r| r.provider_id == "google"));
    }

    #[test]
    fn render_anthropic_header_is_raw_key() {
        let rule = &default_rules()[0];
        assert_eq!(rule.provider_id, "anthropic");
        assert_eq!(rule.render_header_value("sk-real"), "sk-real");
    }

    #[test]
    fn render_openai_header_uses_bearer_prefix() {
        let rule = default_rules()
            .into_iter()
            .find(|r| r.provider_id == "openai")
            .unwrap();
        assert_eq!(rule.render_header_value("sk-real"), "Bearer sk-real");
    }

    #[test]
    fn match_host_is_case_insensitive() {
        let cfg = AuthGatewayConfig::with_default_rules();
        assert!(cfg.match_host("api.anthropic.com").is_some());
        assert!(cfg.match_host("API.ANTHROPIC.COM").is_some());
        assert!(cfg.match_host("Api.Anthropic.Com").is_some());
    }

    #[test]
    fn match_host_returns_none_for_unknown_host() {
        let cfg = AuthGatewayConfig::with_default_rules();
        assert!(cfg.match_host("evil.example.com").is_none());
    }

    #[test]
    fn match_host_returns_first_matching_rule() {
        let mut cfg = AuthGatewayConfig::with_default_rules();
        // Prepend a duplicate hostname with a different provider — match
        // should return the first one so operators can override defaults
        // by prepending rules.
        cfg.rules.insert(
            0,
            AuthRule {
                hostname: "api.anthropic.com".to_string(),
                provider_id: "anthropic-override".to_string(),
                env_key: "ANTHROPIC_API_KEY".to_string(),
                header_name: "x-api-key".to_string(),
                header_format: "{key}".to_string(),
                strip_headers: vec![],
                upstream_scheme: "https".to_string(),
                upstream_authority: None,
            },
        );
        assert_eq!(
            cfg.match_host("api.anthropic.com").unwrap().provider_id,
            "anthropic-override"
        );
    }

    #[test]
    fn strip_headers_defaults_include_authorization_and_x_api_key() {
        let rule = &default_rules()[0];
        let lower: Vec<_> = rule
            .strip_headers
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        assert!(lower.iter().any(|h| h == "authorization"));
        assert!(lower.iter().any(|h| h == "x-api-key"));
    }

    #[test]
    fn miss_policy_default_is_reject() {
        assert_eq!(MissPolicy::default(), MissPolicy::Reject);
    }

    #[test]
    fn default_bind_is_loopback_ephemeral() {
        let cfg = AuthGatewayConfig::default();
        assert_eq!(cfg.bind.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(cfg.bind.port(), 0);
    }

    #[test]
    fn config_roundtrips_through_json() {
        let cfg = AuthGatewayConfig::with_default_rules();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AuthGatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rules, cfg.rules);
        assert_eq!(back.on_miss, cfg.on_miss);
        assert_eq!(back.bind, cfg.bind);
        assert_eq!(back.firewall, cfg.firewall);
    }

    #[test]
    fn config_without_firewall_key_keeps_restrictive_default() {
        // Configs written by pre-5f builds won't have a `firewall` key.
        // They must still deserialize, with the firewall locked down.
        let cfg: AuthGatewayConfig =
            serde_json::from_str(r#"{"rules":[],"on_miss":"reject"}"#).unwrap();
        assert!(!cfg.firewall.allow_private_upstreams);
    }

    #[test]
    fn dummy_api_key_sentinel_is_stable() {
        assert_eq!(DUMMY_API_KEY, "sk-cuartel-gateway");
    }
}
