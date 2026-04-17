//! Rivet AgentOS network action client.
//!
//! Phase 5e of `SPEC.md`: port forwarding (sandbox→host and host→sandbox)
//! and HTTP proxying through the VM (`vmFetch`). Follows the same
//! `POST /gateway/{actor_id}/action/{name}` pattern used by
//! [`crate::filesystem`] and [`crate::client`].

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::client::RivetClient;

/// Direction of a port forward rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortForwardDirection {
    /// VM process connects to a port that is tunnelled to the host.
    SandboxToHost,
    /// Host process connects to a port that is tunnelled into the VM.
    HostToSandbox,
}

impl PortForwardDirection {
    pub fn label(&self) -> &'static str {
        match self {
            Self::SandboxToHost => "sandbox \u{2192} host",
            Self::HostToSandbox => "host \u{2192} sandbox",
        }
    }
}

impl std::fmt::Display for PortForwardDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Configuration for a single port forward rule sent to the rivetkit actor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardConfig {
    pub direction: PortForwardDirection,
    pub sandbox_port: u16,
    pub host_port: u16,
}

/// An active port forward rule returned by the rivetkit actor.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardEntry {
    pub id: String,
    pub direction: PortForwardDirection,
    pub sandbox_port: u16,
    pub host_port: u16,
    pub active: bool,
}

/// Options for an HTTP request proxied through the VM via `vmFetch`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmFetchOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl VmFetchOptions {
    fn is_empty(&self) -> bool {
        self.method.is_none() && self.headers.is_none() && self.body.is_none()
    }
}

/// Response from a `vmFetch` proxied HTTP request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmFetchResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: String,
}

impl RivetClient {
    /// Proxy an HTTP request through the VM. Maps to the `vmFetch(url, options?)`
    /// action in `buildNetworkActions`.
    pub async fn vm_fetch(
        &self,
        actor_id: &str,
        url: &str,
        options: VmFetchOptions,
    ) -> Result<VmFetchResponse> {
        let args = build_vm_fetch_args(url, &options);
        self.call_action(actor_id, "vmFetch", args).await
    }

    /// Register a port forward rule on the given actor. Maps to
    /// `addPortForward(config)`.
    pub async fn add_port_forward(
        &self,
        actor_id: &str,
        config: &PortForwardConfig,
    ) -> Result<PortForwardEntry> {
        let args = vec![
            serde_json::to_value(config).expect("PortForwardConfig is serializable"),
        ];
        self.call_action(actor_id, "addPortForward", args).await
    }

    /// Remove an active port forward rule. Maps to `removePortForward(id)`.
    pub async fn remove_port_forward(
        &self,
        actor_id: &str,
        forward_id: &str,
    ) -> Result<()> {
        let args = vec![Value::String(forward_id.to_string())];
        let _: Option<Value> = self
            .call_action(actor_id, "removePortForward", args)
            .await?;
        Ok(())
    }

    /// List all active port forward rules on the actor. Maps to
    /// `listPortForwards()`.
    pub async fn list_port_forwards(
        &self,
        actor_id: &str,
    ) -> Result<Vec<PortForwardEntry>> {
        self.call_action(actor_id, "listPortForwards", vec![]).await
    }
}

// --- Pure helpers (easy to unit test) ----------------------------------------

fn build_vm_fetch_args(url: &str, options: &VmFetchOptions) -> Vec<Value> {
    let mut args = vec![Value::String(url.to_string())];
    if !options.is_empty() {
        args.push(serde_json::to_value(options).expect("VmFetchOptions is serializable"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn direction_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(PortForwardDirection::SandboxToHost).unwrap(),
            json!("sandbox_to_host"),
        );
        assert_eq!(
            serde_json::to_value(PortForwardDirection::HostToSandbox).unwrap(),
            json!("host_to_sandbox"),
        );
    }

    #[test]
    fn direction_round_trips() {
        let d = PortForwardDirection::HostToSandbox;
        let s = serde_json::to_string(&d).unwrap();
        let back: PortForwardDirection = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn direction_display() {
        assert_eq!(
            PortForwardDirection::SandboxToHost.to_string(),
            "sandbox \u{2192} host",
        );
        assert_eq!(
            PortForwardDirection::HostToSandbox.to_string(),
            "host \u{2192} sandbox",
        );
    }

    #[test]
    fn port_forward_config_serializes_camel_case() {
        let config = PortForwardConfig {
            direction: PortForwardDirection::HostToSandbox,
            sandbox_port: 3000,
            host_port: 8080,
        };
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(
            value,
            json!({
                "direction": "host_to_sandbox",
                "sandboxPort": 3000,
                "hostPort": 8080,
            }),
        );
    }

    #[test]
    fn port_forward_entry_deserializes() {
        let value = json!({
            "id": "pf-abc123",
            "direction": "sandbox_to_host",
            "sandboxPort": 5432,
            "hostPort": 5432,
            "active": true,
        });
        let entry: PortForwardEntry = serde_json::from_value(value).unwrap();
        assert_eq!(entry.id, "pf-abc123");
        assert_eq!(entry.direction, PortForwardDirection::SandboxToHost);
        assert_eq!(entry.sandbox_port, 5432);
        assert_eq!(entry.host_port, 5432);
        assert!(entry.active);
    }

    #[test]
    fn vm_fetch_args_url_only() {
        let args = build_vm_fetch_args(
            "http://localhost:3000/api",
            &VmFetchOptions::default(),
        );
        assert_eq!(args, vec![json!("http://localhost:3000/api")]);
    }

    #[test]
    fn vm_fetch_args_with_method_and_headers() {
        let opts = VmFetchOptions {
            method: Some("POST".into()),
            headers: Some(HashMap::from([(
                "Content-Type".into(),
                "application/json".into(),
            )])),
            body: Some(r#"{"key": "value"}"#.into()),
        };
        let args = build_vm_fetch_args("http://localhost:3000/api", &opts);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], json!("http://localhost:3000/api"));
        assert_eq!(args[1]["method"], json!("POST"));
        assert_eq!(
            args[1]["headers"]["Content-Type"],
            json!("application/json"),
        );
    }

    #[test]
    fn vm_fetch_options_omit_none_fields() {
        let opts = VmFetchOptions {
            method: Some("GET".into()),
            ..Default::default()
        };
        let value = serde_json::to_value(&opts).unwrap();
        assert!(value.get("headers").is_none());
        assert!(value.get("body").is_none());
        assert_eq!(value["method"], json!("GET"));
    }

    #[test]
    fn vm_fetch_response_deserializes_with_defaults() {
        let value = json!({ "status": 200 });
        let resp: VmFetchResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.headers.is_empty());
        assert!(resp.body.is_empty());
    }

    #[test]
    fn vm_fetch_response_deserializes_full() {
        let value = json!({
            "status": 201,
            "headers": { "Content-Type": "application/json" },
            "body": "{\"ok\":true}",
        });
        let resp: VmFetchResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.status, 201);
        assert_eq!(
            resp.headers.get("Content-Type").unwrap(),
            "application/json",
        );
        assert_eq!(resp.body, "{\"ok\":true}");
    }
}
