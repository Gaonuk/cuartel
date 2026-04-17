//! Network firewall rules ensuring VMs cannot reach credential storage
//! or other sensitive host resources (spec task 5f).
//!
//! The auth gateway (5c) blocks requests to unrecognized hosts via
//! `MissPolicy::Reject`. This module formalizes the protection boundary
//! by defining which host endpoints are off-limits to VMs and providing
//! validation hooks for port forwards and gateway configuration.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use cuartel_rivet::network::{PortForwardConfig, PortForwardDirection};

use crate::auth_gateway::{AuthGatewayConfig, MissPolicy};

/// A host resource that VMs must never reach directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedEndpoint {
    pub label: String,
    pub kind: ProtectedKind,
    pub reason: String,
}

/// What kind of host resource is protected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProtectedKind {
    /// A TCP port on the loopback interface.
    LoopbackPort { port: u16 },
    /// A filesystem path (e.g. the SQLite database).
    FilePath { path: PathBuf },
}

/// Outcome of a firewall policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallVerdict {
    Allow,
    Deny { endpoint: String, reason: String },
}

impl FirewallVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn is_denied(&self) -> bool {
        !self.is_allowed()
    }
}

impl std::fmt::Display for FirewallVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allow => f.write_str("allow"),
            Self::Deny { endpoint, reason } => {
                write!(f, "deny ({endpoint}): {reason}")
            }
        }
    }
}

/// Network isolation policy for VM→host traffic.
///
/// Collects protected endpoints and validates that VM network operations
/// (port forwards, gateway config) don't violate the security boundary.
/// The auth gateway's `MissPolicy::Reject` is the runtime enforcement;
/// this struct provides the policy definition and pre-flight validation.
pub struct NetworkPolicy {
    protected: Vec<ProtectedEndpoint>,
}

impl NetworkPolicy {
    /// Build a policy from the current infrastructure state.
    ///
    /// Protected endpoints:
    /// - Auth gateway port — VMs route through it via `*_BASE_URL`, raw
    ///   port forwarding would bypass credential injection.
    /// - Rivet API port — the VM management API must not be reachable
    ///   from within VMs.
    /// - SQLite database path — credentials at rest.
    pub fn new(
        gateway_addr: Option<SocketAddr>,
        rivet_port: u16,
        db_path: Option<PathBuf>,
    ) -> Self {
        let mut protected = Vec::new();

        if let Some(addr) = gateway_addr {
            protected.push(ProtectedEndpoint {
                label: "Auth Gateway".into(),
                kind: ProtectedKind::LoopbackPort {
                    port: addr.port(),
                },
                reason: "raw port forward to the gateway would bypass credential injection"
                    .into(),
            });
        }

        protected.push(ProtectedEndpoint {
            label: "Rivet API".into(),
            kind: ProtectedKind::LoopbackPort { port: rivet_port },
            reason: "VM management API must not be accessible from within VMs".into(),
        });

        if let Some(path) = db_path {
            protected.push(ProtectedEndpoint {
                label: "Credential Database".into(),
                kind: ProtectedKind::FilePath { path },
                reason: "encrypted credentials at rest must not be readable by VMs".into(),
            });
        }

        Self { protected }
    }

    /// Check whether a port forward is safe to create.
    ///
    /// Host→sandbox forwards are always allowed (the host initiates the
    /// connection). Sandbox→host forwards are checked against the
    /// protected port set — a VM must not be able to connect to the
    /// gateway, the Rivet API, or any other protected host service.
    pub fn check_port_forward(&self, config: &PortForwardConfig) -> FirewallVerdict {
        if config.direction == PortForwardDirection::HostToSandbox {
            return FirewallVerdict::Allow;
        }

        for ep in &self.protected {
            if let ProtectedKind::LoopbackPort { port } = &ep.kind {
                if config.host_port == *port {
                    return FirewallVerdict::Deny {
                        endpoint: ep.label.clone(),
                        reason: ep.reason.clone(),
                    };
                }
            }
        }

        FirewallVerdict::Allow
    }

    /// Validate that the gateway configuration enforces the security model.
    ///
    /// Two invariants:
    /// 1. `MissPolicy` must be `Reject` — `Passthrough` lets VMs reach
    ///    arbitrary hosts without credential injection.
    /// 2. The bind address must be loopback — binding to a routable
    ///    interface exposes the credential injection proxy to the network.
    pub fn validate_gateway_config(config: &AuthGatewayConfig) -> FirewallVerdict {
        if config.on_miss != MissPolicy::Reject {
            return FirewallVerdict::Deny {
                endpoint: "Auth Gateway".into(),
                reason: "MissPolicy must be Reject; Passthrough allows VMs to reach arbitrary hosts".into(),
            };
        }

        if !config.bind.ip().is_loopback() {
            return FirewallVerdict::Deny {
                endpoint: "Auth Gateway".into(),
                reason: format!(
                    "gateway must bind to loopback, not {}; a routable address exposes credential injection to the network",
                    config.bind.ip()
                ),
            };
        }

        FirewallVerdict::Allow
    }

    /// The set of protected endpoints, for display in settings/debug UI.
    pub fn protected_endpoints(&self) -> &[ProtectedEndpoint] {
        &self.protected
    }

    /// Protected port numbers on the loopback interface.
    pub fn protected_ports(&self) -> Vec<u16> {
        self.protected
            .iter()
            .filter_map(|ep| match &ep.kind {
                ProtectedKind::LoopbackPort { port } => Some(*port),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy() -> NetworkPolicy {
        NetworkPolicy::new(
            Some("127.0.0.1:9999".parse().unwrap()),
            6420,
            Some(PathBuf::from("/data/cuartel.db")),
        )
    }

    #[test]
    fn protected_ports_include_gateway_and_rivet() {
        let policy = test_policy();
        let ports = policy.protected_ports();
        assert!(ports.contains(&9999), "gateway port must be protected");
        assert!(ports.contains(&6420), "rivet port must be protected");
    }

    #[test]
    fn protected_endpoints_include_db_path() {
        let policy = test_policy();
        let has_db = policy.protected_endpoints().iter().any(|ep| {
            matches!(&ep.kind, ProtectedKind::FilePath { path } if path.to_str() == Some("/data/cuartel.db"))
        });
        assert!(has_db, "database path must be in protected endpoints");
    }

    #[test]
    fn host_to_sandbox_always_allowed() {
        let policy = test_policy();
        let config = PortForwardConfig {
            direction: PortForwardDirection::HostToSandbox,
            sandbox_port: 3000,
            host_port: 6420,
        };
        assert!(policy.check_port_forward(&config).is_allowed());
    }

    #[test]
    fn sandbox_to_host_denied_for_gateway_port() {
        let policy = test_policy();
        let config = PortForwardConfig {
            direction: PortForwardDirection::SandboxToHost,
            sandbox_port: 9999,
            host_port: 9999,
        };
        let verdict = policy.check_port_forward(&config);
        assert!(verdict.is_denied());
        match &verdict {
            FirewallVerdict::Deny { endpoint, .. } => {
                assert_eq!(endpoint, "Auth Gateway");
            }
            _ => panic!("expected Deny"),
        }
    }

    #[test]
    fn sandbox_to_host_denied_for_rivet_port() {
        let policy = test_policy();
        let config = PortForwardConfig {
            direction: PortForwardDirection::SandboxToHost,
            sandbox_port: 6420,
            host_port: 6420,
        };
        let verdict = policy.check_port_forward(&config);
        assert!(verdict.is_denied());
        match &verdict {
            FirewallVerdict::Deny { endpoint, .. } => {
                assert_eq!(endpoint, "Rivet API");
            }
            _ => panic!("expected Deny"),
        }
    }

    #[test]
    fn sandbox_to_host_allowed_for_unprotected_port() {
        let policy = test_policy();
        let config = PortForwardConfig {
            direction: PortForwardDirection::SandboxToHost,
            sandbox_port: 3000,
            host_port: 8080,
        };
        assert!(policy.check_port_forward(&config).is_allowed());
    }

    #[test]
    fn gateway_config_reject_loopback_passes() {
        let config = AuthGatewayConfig::with_default_rules();
        assert!(NetworkPolicy::validate_gateway_config(&config).is_allowed());
    }

    #[test]
    fn gateway_config_passthrough_fails() {
        let mut config = AuthGatewayConfig::with_default_rules();
        config.on_miss = MissPolicy::Passthrough;
        let verdict = NetworkPolicy::validate_gateway_config(&config);
        assert!(verdict.is_denied());
        assert!(
            format!("{verdict}").contains("Passthrough"),
            "message should mention Passthrough"
        );
    }

    #[test]
    fn gateway_config_non_loopback_fails() {
        let mut config = AuthGatewayConfig::with_default_rules();
        config.bind = "0.0.0.0:0".parse().unwrap();
        let verdict = NetworkPolicy::validate_gateway_config(&config);
        assert!(verdict.is_denied());
        assert!(
            format!("{verdict}").contains("loopback"),
            "message should mention loopback"
        );
    }

    #[test]
    fn policy_without_gateway_still_protects_rivet() {
        let policy = NetworkPolicy::new(None, 6420, None);
        let ports = policy.protected_ports();
        assert_eq!(ports, vec![6420]);
    }

    #[test]
    fn verdict_display() {
        assert_eq!(FirewallVerdict::Allow.to_string(), "allow");
        let deny = FirewallVerdict::Deny {
            endpoint: "Test".into(),
            reason: "not allowed".into(),
        };
        assert_eq!(deny.to_string(), "deny (Test): not allowed");
    }

    #[test]
    fn protected_endpoint_roundtrips_through_json() {
        let ep = ProtectedEndpoint {
            label: "Rivet API".into(),
            kind: ProtectedKind::LoopbackPort { port: 6420 },
            reason: "management API".into(),
        };
        let json = serde_json::to_string(&ep).unwrap();
        let back: ProtectedEndpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.label, ep.label);
        match back.kind {
            ProtectedKind::LoopbackPort { port } => assert_eq!(port, 6420),
            _ => panic!("expected LoopbackPort"),
        }
    }

    #[test]
    fn file_path_endpoint_roundtrips_through_json() {
        let ep = ProtectedEndpoint {
            label: "DB".into(),
            kind: ProtectedKind::FilePath {
                path: PathBuf::from("/tmp/test.db"),
            },
            reason: "credentials".into(),
        };
        let json = serde_json::to_string(&ep).unwrap();
        let back: ProtectedEndpoint = serde_json::from_str(&json).unwrap();
        match back.kind {
            ProtectedKind::FilePath { path } => {
                assert_eq!(path, PathBuf::from("/tmp/test.db"))
            }
            _ => panic!("expected FilePath"),
        }
    }
}
