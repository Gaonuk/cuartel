//! Audit events emitted by the auth gateway.
//!
//! Phase 5c emits events into a `tokio::sync::broadcast` channel. Phase 5d
//! (audit-log persistence) adds a subscriber that writes each event to a
//! SQLite table. Nothing in 5c needs the data to reach durable storage, so
//! we intentionally stop at an in-memory fan-out — a dropped event in the
//! broadcast backlog is acceptable given audit is a diagnostic aid, not a
//! load-bearing security control.

use std::net::IpAddr;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// A single audit record produced by the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    /// A credential was successfully injected and the upstream request
    /// completed. Emitted *after* the response status is known so the log
    /// captures the upstream outcome without holding the body open.
    Injected {
        timestamp: SystemTime,
        client_ip: Option<IpAddr>,
        hostname: String,
        provider_id: String,
        env_key: String,
        method: String,
        path: String,
        status: u16,
    },
    /// An incoming request's host did not match any rule and the gateway's
    /// `MissPolicy` was `Reject`. This is the primary signal that an agent
    /// tried to talk to a host it shouldn't.
    Blocked {
        timestamp: SystemTime,
        client_ip: Option<IpAddr>,
        hostname: String,
        method: String,
        path: String,
        reason: String,
    },
    /// A rule matched but the credential store had no value for
    /// `(provider_id, env_key)`. Usually means the user hasn't finished
    /// onboarding for that provider.
    CredentialMissing {
        timestamp: SystemTime,
        hostname: String,
        provider_id: String,
        env_key: String,
    },
    /// The gateway could not complete the upstream leg of a request
    /// (DNS, TLS, connection reset, etc.).
    UpstreamError {
        timestamp: SystemTime,
        hostname: String,
        provider_id: String,
        error: String,
    },
}

impl AuditEvent {
    /// Convenience for callers that only need the top-level variant name
    /// (e.g. for metrics / grouping). Keeps call sites free of a `match`
    /// ladder when all they want is a string label.
    pub fn kind(&self) -> &'static str {
        match self {
            AuditEvent::Injected { .. } => "injected",
            AuditEvent::Blocked { .. } => "blocked",
            AuditEvent::CredentialMissing { .. } => "credential_missing",
            AuditEvent::UpstreamError { .. } => "upstream_error",
        }
    }
}

/// Shared sender type used by the proxy to emit events. Wrapped in an
/// `Arc` by the caller so clones are cheap.
pub type AuditSender = tokio::sync::broadcast::Sender<AuditEvent>;

/// Default broadcast buffer. Oversized on purpose — the gateway emits at
/// most one event per proxied request, so 256 slots give subscribers ~2–3
/// seconds of headroom under a burst at human-driven volumes.
pub const DEFAULT_AUDIT_BUFFER: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_covers_every_variant() {
        let now = SystemTime::now();
        assert_eq!(
            AuditEvent::Injected {
                timestamp: now,
                client_ip: None,
                hostname: "x".into(),
                provider_id: "p".into(),
                env_key: "K".into(),
                method: "GET".into(),
                path: "/".into(),
                status: 200,
            }
            .kind(),
            "injected"
        );
        assert_eq!(
            AuditEvent::Blocked {
                timestamp: now,
                client_ip: None,
                hostname: "x".into(),
                method: "GET".into(),
                path: "/".into(),
                reason: "no rule".into(),
            }
            .kind(),
            "blocked"
        );
        assert_eq!(
            AuditEvent::CredentialMissing {
                timestamp: now,
                hostname: "x".into(),
                provider_id: "p".into(),
                env_key: "K".into(),
            }
            .kind(),
            "credential_missing"
        );
        assert_eq!(
            AuditEvent::UpstreamError {
                timestamp: now,
                hostname: "x".into(),
                provider_id: "p".into(),
                error: "boom".into(),
            }
            .kind(),
            "upstream_error"
        );
    }
}
