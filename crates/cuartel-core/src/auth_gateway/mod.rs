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

mod rules;

pub use rules::{default_rules, AuthGatewayConfig, AuthRule, MissPolicy, DUMMY_API_KEY};
