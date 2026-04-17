//! Bridges the SQLite-backed server registry ([`cuartel_db::servers`]) with
//! live Tailscale discovery ([`crate::tailscale`]).
//!
//! The app reads the persistent list of servers from the DB — that's the
//! source of truth for "what cuartel should talk to". The tailnet snapshot
//! is advisory: it lets the user pick a reachable peer to register, and
//! drives the reachability badge in the sidebar. Tailscale state is NEVER
//! written back to the DB automatically; the user opts in per peer.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use cuartel_db::servers::ServerRepo;
use cuartel_db::Database;
use cuartel_rivet::client::RivetClient;

use crate::server::RemoteServer;
use crate::tailscale::{TailnetSnapshot, TailscaleClient, TailscaleDevice};

/// The default port rivet listens on inside a remote cuartel install. Kept
/// here (rather than in cuartel-rivet) so the registry can build URLs from
/// a bare Tailscale IP without pulling a hard dep on the rivet crate.
pub const DEFAULT_RIVET_PORT: u16 = 6420;

#[derive(Clone)]
pub struct ServerRegistry {
    db: Arc<Mutex<Database>>,
    tailscale: Arc<TailscaleClient>,
}

impl ServerRegistry {
    pub fn new(db: Arc<Mutex<Database>>, tailscale: Arc<TailscaleClient>) -> Self {
        Self { db, tailscale }
    }

    /// Ensure the built-in `local` row exists with the given rivet base URL.
    /// Must be called at startup so the sidebar can render something even
    /// before the user registers any peers.
    pub fn ensure_local(&self, address: &str) -> Result<RemoteServer> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("server registry mutex poisoned"))?;
        let row = ServerRepo::new(&db).ensure_local(address)?;
        Ok(RemoteServer::from(row))
    }

    pub fn list(&self) -> Result<Vec<RemoteServer>> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("server registry mutex poisoned"))?;
        let rows = ServerRepo::new(&db).list()?;
        Ok(rows.into_iter().map(RemoteServer::from).collect())
    }

    pub fn get(&self, id: &str) -> Result<Option<RemoteServer>> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("server registry mutex poisoned"))?;
        Ok(ServerRepo::new(&db).get(id)?.map(RemoteServer::from))
    }

    /// Register a Tailscale peer as a remote server. The `name` overrides
    /// the device hostname (users often rename "ubuntu-4gb-hel1-1" to
    /// something memorable); if empty, falls back to the device hostname.
    /// Returns the freshly-inserted row. Errors if the peer has no routable
    /// address, or a server with the same tailscale IP is already registered.
    pub fn register_peer(
        &self,
        id: &str,
        name: &str,
        device: &TailscaleDevice,
    ) -> Result<RemoteServer> {
        let Some(ip) = device.primary_address() else {
            return Err(anyhow!(
                "tailscale device '{}' has no routable address",
                device.hostname
            ));
        };
        let resolved_name = if name.trim().is_empty() {
            device.hostname.clone()
        } else {
            name.to_string()
        };
        let address = format!("http://{ip}:{DEFAULT_RIVET_PORT}");

        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("server registry mutex poisoned"))?;
        let repo = ServerRepo::new(&db);
        if repo.find_by_tailscale_ip(ip)?.is_some() {
            return Err(anyhow!("peer {ip} is already registered"));
        }
        let row = repo.insert(id, &resolved_name, &address, Some(ip), false)?;
        Ok(RemoteServer::from(row))
    }

    /// Update a registered server's display name and address.
    pub fn update(
        &self,
        id: &str,
        name: &str,
        address: &str,
        tailscale_ip: Option<&str>,
    ) -> Result<RemoteServer> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("server registry mutex poisoned"))?;
        let row = ServerRepo::new(&db).update(id, name, address, tailscale_ip)?;
        Ok(RemoteServer::from(row))
    }

    /// Delete a registered server. Refuses to delete the built-in local row.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("server registry mutex poisoned"))?;
        ServerRepo::new(&db).delete(id)
    }

    /// Snapshot the current tailnet. Cheap: shells out to the local CLI.
    pub async fn snapshot_tailnet(&self) -> TailnetSnapshot {
        self.tailscale.snapshot().await
    }

    /// HTTP-probe a server's rivet endpoint. Returns `false` on any error.
    pub async fn check_reachability(&self, server: &RemoteServer) -> bool {
        let target = server
            .tailscale_ip
            .as_deref()
            .unwrap_or_else(|| host_from_address(&server.address));
        self.tailscale
            .check_connectivity(target, DEFAULT_RIVET_PORT)
            .await
            .unwrap_or(false)
    }
}

/// Extract the host portion of an `http://host:port` URL without pulling in
/// a full URL parser for what is always a well-formed rivet address.
fn host_from_address(address: &str) -> &str {
    let without_scheme = address
        .strip_prefix("http://")
        .or_else(|| address.strip_prefix("https://"))
        .unwrap_or(address);
    match without_scheme.find(':') {
        Some(i) => &without_scheme[..i],
        None => without_scheme.split('/').next().unwrap_or(without_scheme),
    }
}

/// Convenience — used by callers that need to quickly derive the local
/// rivet base URL for a port chosen at startup.
pub fn local_base_url(port: u16) -> String {
    format!("http://localhost:{port}")
}

/// Build a [`RivetClient`] pointed at a registered server (phase 7d).
///
/// Lives here — not in cuartel-rivet — so cuartel-rivet stays unaware of the
/// registry / Tailscale layer above it.
pub fn rivet_client_for(server: &RemoteServer) -> RivetClient {
    RivetClient::new(server.rivet_url())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuartel_db::servers::{ServerRow, LOCAL_SERVER_ID};

    fn registry() -> ServerRegistry {
        let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
        let ts = Arc::new(TailscaleClient::new());
        ServerRegistry::new(db, ts)
    }

    fn device(host: &str, ip: &str) -> TailscaleDevice {
        TailscaleDevice {
            hostname: host.into(),
            dns_name: format!("{host}.ts.net"),
            addresses: vec![ip.into()],
            os: "linux".into(),
            online: true,
            is_self: false,
        }
    }

    #[test]
    fn ensure_local_seeds_the_registry() {
        let reg = registry();
        let local = reg.ensure_local("http://localhost:6420").unwrap();
        assert_eq!(local.id, LOCAL_SERVER_ID);
        assert!(local.is_local);
        let listed = reg.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, LOCAL_SERVER_ID);
    }

    #[test]
    fn register_peer_inserts_remote_row() {
        let reg = registry();
        reg.ensure_local("http://localhost:6420").unwrap();
        let dev = device("hetzner-1", "100.67.106.62");
        let registered = reg
            .register_peer("hetzner-1", "Hetzner", &dev)
            .unwrap();
        assert_eq!(registered.name, "Hetzner");
        assert_eq!(registered.address, "http://100.67.106.62:6420");
        assert_eq!(registered.tailscale_ip.as_deref(), Some("100.67.106.62"));
        assert!(!registered.is_local);

        let listed = reg.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].is_local); // local stays on top
    }

    #[test]
    fn register_peer_falls_back_to_hostname_when_name_blank() {
        let reg = registry();
        let dev = device("ubuntu-hel1-1", "100.67.106.62");
        let registered = reg.register_peer("h1", "  ", &dev).unwrap();
        assert_eq!(registered.name, "ubuntu-hel1-1");
    }

    #[test]
    fn register_peer_rejects_duplicate_ip() {
        let reg = registry();
        let dev = device("h", "100.67.106.62");
        reg.register_peer("h1", "H1", &dev).unwrap();
        let err = reg.register_peer("h2", "H2", &dev).unwrap_err();
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn register_peer_rejects_device_without_address() {
        let reg = registry();
        let dev = TailscaleDevice {
            hostname: "empty".into(),
            dns_name: "empty.ts.net".into(),
            addresses: vec![],
            os: "linux".into(),
            online: true,
            is_self: false,
        };
        assert!(reg.register_peer("h", "Empty", &dev).is_err());
    }

    #[test]
    fn delete_refuses_local_row() {
        let reg = registry();
        reg.ensure_local("http://localhost:6420").unwrap();
        assert!(reg.delete(LOCAL_SERVER_ID).is_err());
    }

    #[test]
    fn delete_removes_remote_row() {
        let reg = registry();
        let dev = device("h", "100.67.106.62");
        reg.register_peer("h1", "H1", &dev).unwrap();
        assert!(reg.delete("h1").unwrap());
        assert!(reg.get("h1").unwrap().is_none());
    }

    #[test]
    fn update_changes_metadata() {
        let reg = registry();
        let dev = device("h", "100.67.106.62");
        reg.register_peer("h1", "H1", &dev).unwrap();
        let updated = reg
            .update("h1", "New H1", "http://new:6420", Some("100.0.0.99"))
            .unwrap();
        assert_eq!(updated.name, "New H1");
        assert_eq!(updated.address, "http://new:6420");
        assert_eq!(updated.tailscale_ip.as_deref(), Some("100.0.0.99"));
    }

    #[test]
    fn host_from_address_parses_common_shapes() {
        assert_eq!(host_from_address("http://localhost:6420"), "localhost");
        assert_eq!(host_from_address("http://100.67.106.62:6420"), "100.67.106.62");
        assert_eq!(host_from_address("https://example.com"), "example.com");
        assert_eq!(host_from_address("100.67.106.62"), "100.67.106.62");
    }

    #[test]
    fn local_base_url_formats_port() {
        assert_eq!(local_base_url(6420), "http://localhost:6420");
        assert_eq!(local_base_url(9000), "http://localhost:9000");
    }

    #[test]
    fn rivet_client_for_local_uses_localhost() {
        let local = RemoteServer::local_default();
        let client = rivet_client_for(&local);
        assert_eq!(client.base_url(), "http://localhost:6420");
    }

    #[test]
    fn rivet_client_for_remote_uses_tailscale_address() {
        let dev = device("hetzner-1", "100.67.106.62");
        let reg = registry();
        let remote = reg.register_peer("h1", "Hetzner", &dev).unwrap();
        let client = rivet_client_for(&remote);
        assert_eq!(client.base_url(), "http://100.67.106.62:6420");
    }

    #[test]
    fn register_peer_with_row_conversion_shape() {
        let reg = registry();
        let dev = device("h", "100.67.106.62");
        let registered = reg.register_peer("h1", "H1", &dev).unwrap();
        // Round trip through ServerRow.
        let row: Option<ServerRow> = {
            let db = reg.db.lock().unwrap();
            ServerRepo::new(&db).get("h1").unwrap()
        };
        let row = row.unwrap();
        assert_eq!(row.id, registered.id);
        assert_eq!(row.name, registered.name);
        assert_eq!(row.address, registered.address);
        assert_eq!(row.tailscale_ip, registered.tailscale_ip);
        assert_eq!(row.is_local, registered.is_local);
    }
}
