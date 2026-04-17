//! Auth gateway (spec task 5c).
//!
//! Reverse-proxy that sits between the rivet sidecar's agent subprocesses
//! and upstream LLM providers. Agents talk to the gateway with a dummy
//! credential (`sk-cuartel-gateway`); the gateway looks up the real key in
//! the credential store based on the request's `Host` header, strips any
//! incoming auth headers, and injects the real credential before forwarding.
//!
//! This module is currently **PR-1**: pure types + default rules + tests.
//! The hyper proxy server lands in a follow-up PR (`proxy.rs`).

mod audit;
mod firewall;
mod host;
mod persister;
mod proxy;
mod rules;

pub use audit::{AuditEvent, AuditSender, DEFAULT_AUDIT_BUFFER};
pub use firewall::{is_blocked_ip, parse_ip_authority, FirewallPolicy, BLOCK_REASON_PRIVATE_UPSTREAM};
pub use host::{GatewayHost, GatewayStatus};
pub use persister::{spawn_audit_persister, AuditSink, DatabaseAuditSink};
pub use proxy::{bind, ProxyBody, ProxyBodyError};
pub use rules::{default_rules, AuthGatewayConfig, AuthRule, MissPolicy, DUMMY_API_KEY};
