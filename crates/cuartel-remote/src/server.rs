//! Registered server: either the local sidecar on this Mac, or a remote
//! rivet instance reached over Tailscale.

use cuartel_db::servers::{ServerRow, LOCAL_SERVER_ID};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteServer {
    pub id: String,
    pub name: String,
    pub address: String,
    pub tailscale_ip: Option<String>,
    pub is_local: bool,
}

impl RemoteServer {
    /// Default entry for "this Mac" — used as a fallback when the DB has no
    /// `local` row yet (e.g. during the very first launch).
    pub fn local_default() -> Self {
        Self {
            id: LOCAL_SERVER_ID.to_string(),
            name: "This Mac".to_string(),
            address: "http://localhost:6420".to_string(),
            tailscale_ip: None,
            is_local: true,
        }
    }

    pub fn rivet_url(&self) -> &str {
        &self.address
    }
}

impl From<ServerRow> for RemoteServer {
    fn from(row: ServerRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            address: row.address,
            tailscale_ip: row.tailscale_ip,
            is_local: row.is_local,
        }
    }
}

impl From<&ServerRow> for RemoteServer {
    fn from(row: &ServerRow) -> Self {
        Self {
            id: row.id.clone(),
            name: row.name.clone(),
            address: row.address.clone(),
            tailscale_ip: row.tailscale_ip.clone(),
            is_local: row.is_local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_server_row_preserves_fields() {
        let row = ServerRow {
            id: "hetzner-1".into(),
            name: "Hetzner".into(),
            address: "http://100.67.106.62:6420".into(),
            tailscale_ip: Some("100.67.106.62".into()),
            is_local: false,
            created_at: "2026-01-01 00:00:00".into(),
        };
        let server: RemoteServer = (&row).into();
        assert_eq!(server.id, "hetzner-1");
        assert_eq!(server.name, "Hetzner");
        assert_eq!(server.address, "http://100.67.106.62:6420");
        assert_eq!(server.tailscale_ip.as_deref(), Some("100.67.106.62"));
        assert!(!server.is_local);
    }

    #[test]
    fn local_default_uses_localhost_6420() {
        let local = RemoteServer::local_default();
        assert_eq!(local.id, "local");
        assert!(local.is_local);
        assert_eq!(local.rivet_url(), "http://localhost:6420");
    }
}
