//! Tailscale tailnet discovery and reachability checks.
//!
//! Uses the local `tailscale` CLI (`tailscale status --json`) rather than the
//! hosted control-plane API: the CLI talks to the on-host `tailscaled` over
//! its UNIX socket, so we get up-to-the-second peer info without needing an
//! API key or network round-trip. On hosts where the CLI is missing or the
//! daemon is not running we return `TailnetUnavailable` so callers can render
//! a neutral "no tailnet" state instead of crashing.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

/// A single reachable node on the tailnet (either self or a peer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TailscaleDevice {
    pub hostname: String,
    pub dns_name: String,
    pub addresses: Vec<String>,
    pub os: String,
    pub online: bool,
    pub is_self: bool,
}

impl TailscaleDevice {
    /// Preferred address for reaching this device. IPv4 first (better chance
    /// of working with rivet clients that default to v4), else first IPv6.
    pub fn primary_address(&self) -> Option<&str> {
        self.addresses
            .iter()
            .find(|a| a.parse::<Ipv4Addr>().is_ok())
            .or_else(|| self.addresses.first())
            .map(String::as_str)
    }
}

/// Why `list_devices()` returned an empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailnetStatus {
    /// `tailscale status --json` produced a valid snapshot.
    Available,
    /// Daemon reachable but not logged in / backend not running.
    NotConnected(String),
    /// CLI binary missing or daemon not responding.
    Unavailable(String),
}

#[derive(Debug, Clone)]
pub struct TailnetSnapshot {
    pub devices: Vec<TailscaleDevice>,
    pub status: TailnetStatus,
}

pub struct TailscaleClient {
    http_timeout: Duration,
}

impl Default for TailscaleClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TailscaleClient {
    pub fn new() -> Self {
        Self {
            http_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_http_timeout(mut self, timeout: Duration) -> Self {
        self.http_timeout = timeout;
        self
    }

    /// Shell out to `tailscale status --json` and parse the snapshot.
    pub async fn snapshot(&self) -> TailnetSnapshot {
        match tokio::process::Command::new("tailscale")
            .arg("status")
            .arg("--json")
            .output()
            .await
        {
            Ok(out) if out.status.success() => {
                match parse_status_json(&out.stdout) {
                    Ok(devices) => TailnetSnapshot {
                        devices,
                        status: TailnetStatus::Available,
                    },
                    Err(e) => TailnetSnapshot {
                        devices: vec![],
                        status: TailnetStatus::Unavailable(format!(
                            "failed to parse tailscale status: {e}"
                        )),
                    },
                }
            }
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                TailnetSnapshot {
                    devices: vec![],
                    status: TailnetStatus::NotConnected(if msg.is_empty() {
                        format!("tailscale exited with status {}", out.status)
                    } else {
                        msg
                    }),
                }
            }
            Err(e) => TailnetSnapshot {
                devices: vec![],
                status: TailnetStatus::Unavailable(format!(
                    "failed to spawn tailscale CLI: {e}"
                )),
            },
        }
    }

    /// Convenience: return the peers regardless of status.
    pub async fn list_devices(&self) -> Result<Vec<TailscaleDevice>> {
        Ok(self.snapshot().await.devices)
    }

    /// Probe `http://{ip}:{port}/health` to confirm a rivet sidecar is up.
    /// Returns `Ok(false)` on any connection/timeout error so callers can
    /// render a neutral offline state.
    pub async fn check_connectivity(&self, ip: &str, port: u16) -> Result<bool> {
        let url = format_health_url(ip, port);
        let client = reqwest::Client::builder()
            .timeout(self.http_timeout)
            .build()
            .context("build reqwest client")?;
        match client.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}

fn format_health_url(ip: &str, port: u16) -> String {
    // Wrap bare IPv6 literals in brackets for a well-formed URL.
    if ip.parse::<Ipv4Addr>().is_ok() {
        format!("http://{ip}:{port}/health")
    } else if let Ok(IpAddr::V6(_)) = ip.parse::<IpAddr>() {
        format!("http://[{ip}]:{port}/health")
    } else {
        // Treat as hostname (e.g. MagicDNS name); no bracketing needed.
        format!("http://{ip}:{port}/health")
    }
}

// --- Pure parsing --------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StatusJson {
    #[serde(rename = "Self", default)]
    self_node: Option<StatusNode>,
    #[serde(rename = "Peer", default)]
    peer: std::collections::HashMap<String, StatusNode>,
}

#[derive(Debug, Deserialize)]
struct StatusNode {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: bool,
}

fn parse_status_json(bytes: &[u8]) -> Result<Vec<TailscaleDevice>> {
    let status: StatusJson =
        serde_json::from_slice(bytes).context("decode tailscale status --json")?;
    let mut devices = Vec::new();
    if let Some(s) = status.self_node {
        devices.push(device_from(s, true));
    }
    for (_key, peer) in status.peer {
        devices.push(device_from(peer, false));
    }
    // Sort so the UI gets a stable order: self first, then peers alphabetically.
    devices.sort_by(|a, b| {
        b.is_self
            .cmp(&a.is_self)
            .then_with(|| a.hostname.to_lowercase().cmp(&b.hostname.to_lowercase()))
    });
    Ok(devices)
}

fn device_from(n: StatusNode, is_self: bool) -> TailscaleDevice {
    TailscaleDevice {
        hostname: n.host_name,
        dns_name: n.dns_name.trim_end_matches('.').to_string(),
        addresses: n.tailscale_ips,
        os: n.os,
        online: n.online,
        is_self,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "Version": "1.96.5",
        "BackendState": "Running",
        "Self": {
            "HostName": "my-mac",
            "DNSName": "my-mac.tailf1c74c.ts.net.",
            "OS": "macOS",
            "TailscaleIPs": ["100.91.112.122", "fd7a:115c:a1e0::cf35:707a"],
            "Online": true
        },
        "Peer": {
            "nodekey:aaa": {
                "HostName": "hetzner-1",
                "DNSName": "hetzner-1.tailf1c74c.ts.net.",
                "OS": "linux",
                "TailscaleIPs": ["100.67.106.62"],
                "Online": true
            },
            "nodekey:bbb": {
                "HostName": "Zebra-Pi",
                "DNSName": "zebra-pi.tailf1c74c.ts.net.",
                "OS": "linux",
                "TailscaleIPs": ["100.64.0.5"],
                "Online": false
            }
        }
    }"#;

    #[test]
    fn parses_self_and_peers() {
        let devices = parse_status_json(FIXTURE.as_bytes()).unwrap();
        assert_eq!(devices.len(), 3);

        // Self must come first.
        assert!(devices[0].is_self);
        assert_eq!(devices[0].hostname, "my-mac");
        assert_eq!(devices[0].dns_name, "my-mac.tailf1c74c.ts.net");
        assert_eq!(devices[0].os, "macOS");
        assert_eq!(devices[0].addresses.len(), 2);
        assert!(devices[0].online);

        // Peers sorted alphabetically (case-insensitive): hetzner-1, Zebra-Pi.
        assert!(!devices[1].is_self);
        assert_eq!(devices[1].hostname, "hetzner-1");
        assert!(devices[1].online);

        assert_eq!(devices[2].hostname, "Zebra-Pi");
        assert!(!devices[2].online);
    }

    #[test]
    fn parses_snapshot_without_self() {
        let json = r#"{ "Peer": {} }"#;
        let devices = parse_status_json(json.as_bytes()).unwrap();
        assert!(devices.is_empty());
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_status_json(b"not json").is_err());
    }

    #[test]
    fn primary_address_prefers_ipv4() {
        let dev = TailscaleDevice {
            hostname: "h".into(),
            dns_name: "h".into(),
            addresses: vec![
                "fd7a:115c:a1e0::cf35:707a".into(),
                "100.91.112.122".into(),
            ],
            os: "macOS".into(),
            online: true,
            is_self: true,
        };
        assert_eq!(dev.primary_address(), Some("100.91.112.122"));
    }

    #[test]
    fn primary_address_falls_back_to_ipv6() {
        let dev = TailscaleDevice {
            hostname: "h".into(),
            dns_name: "h".into(),
            addresses: vec!["fd7a:115c:a1e0::cf35:707a".into()],
            os: "linux".into(),
            online: true,
            is_self: false,
        };
        assert_eq!(dev.primary_address(), Some("fd7a:115c:a1e0::cf35:707a"));
    }

    #[test]
    fn primary_address_handles_empty() {
        let dev = TailscaleDevice {
            hostname: "h".into(),
            dns_name: "h".into(),
            addresses: vec![],
            os: "".into(),
            online: false,
            is_self: false,
        };
        assert!(dev.primary_address().is_none());
    }

    #[test]
    fn health_url_ipv4_plain() {
        assert_eq!(
            format_health_url("100.67.106.62", 6420),
            "http://100.67.106.62:6420/health"
        );
    }

    #[test]
    fn health_url_ipv6_bracketed() {
        assert_eq!(
            format_health_url("fd7a:115c:a1e0::cf35:707a", 6420),
            "http://[fd7a:115c:a1e0::cf35:707a]:6420/health"
        );
    }

    #[test]
    fn health_url_hostname_plain() {
        assert_eq!(
            format_health_url("hetzner-1.tailf1c74c.ts.net", 6420),
            "http://hetzner-1.tailf1c74c.ts.net:6420/health"
        );
    }

    #[test]
    fn devices_sorted_self_first() {
        let fixture = r#"{
            "Self": {"HostName":"z-host","TailscaleIPs":[],"Online":true},
            "Peer": {
                "nodekey:a": {"HostName":"aaa","TailscaleIPs":[],"Online":true},
                "nodekey:b": {"HostName":"bbb","TailscaleIPs":[],"Online":true}
            }
        }"#;
        let devices = parse_status_json(fixture.as_bytes()).unwrap();
        assert_eq!(devices[0].hostname, "z-host");
        assert!(devices[0].is_self);
        assert_eq!(devices[1].hostname, "aaa");
        assert_eq!(devices[2].hostname, "bbb");
    }
}
