//! Owns the long-running tokio task that keeps the sidebar's server list
//! in sync with the DB-backed registry + Tailscale reachability probes.
//!
//! The task writes to a shared `Arc<Mutex<Vec<ServerItem>>>`; the sidebar
//! polls that slot on its existing timer loop (same pattern we use for
//! `SidecarStatus`), so there is no extra gpui subscription plumbing.

use std::sync::Arc;
use std::time::Duration;

use cuartel_remote::{RemoteServer, ServerRegistry};
use parking_lot::Mutex;
use tokio::runtime::Handle;

use crate::sidebar::ServerItem;

/// How often we re-scan the registry + re-probe reachability. 15s is long
/// enough to avoid hammering the network and short enough that "I just
/// turned on the Hetzner box" is visible before the user looks again.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

pub struct ServerRegistryHost {
    state: Arc<Mutex<Vec<ServerItem>>>,
    _driver: tokio::task::JoinHandle<()>,
}

impl ServerRegistryHost {
    /// Snapshot the registry synchronously and spawn a background task that
    /// refreshes reachability periodically.
    pub fn spawn(runtime: Handle, registry: Arc<ServerRegistry>) -> Self {
        let initial_items = registry
            .list()
            .map(|servers| {
                servers
                    .into_iter()
                    .map(|s| server_to_item(&s, None))
                    .collect()
            })
            .unwrap_or_else(|e| {
                log::warn!("[server-registry] initial list failed: {e}");
                Vec::new()
            });
        let state = Arc::new(Mutex::new(initial_items));
        let driver_state = state.clone();
        let driver = runtime.spawn(run_driver(registry, driver_state));
        Self {
            state,
            _driver: driver,
        }
    }

    pub fn state(&self) -> Arc<Mutex<Vec<ServerItem>>> {
        self.state.clone()
    }
}

async fn run_driver(registry: Arc<ServerRegistry>, state: Arc<Mutex<Vec<ServerItem>>>) {
    loop {
        // Give the sidecar a moment to boot before the first probe; keeps
        // the initial "unreachable" flash off-screen when the local row
        // exists but the sidecar is still installing deps.
        tokio::time::sleep(POLL_INTERVAL).await;

        let servers = match registry.list() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[server-registry] list failed: {e}");
                continue;
            }
        };

        let mut items = Vec::with_capacity(servers.len());
        for server in &servers {
            // The local row's "reachable" is driven by the sidecar status
            // (which the sidebar already renders); we still set None here
            // to signal "rendering should fall through to sidecar_status".
            let reachable = if server.is_local {
                None
            } else {
                Some(registry.check_reachability(server).await)
            };
            items.push(server_to_item(server, reachable));
        }

        *state.lock() = items;
    }
}

fn server_to_item(server: &RemoteServer, reachable: Option<bool>) -> ServerItem {
    ServerItem {
        id: server.id.clone().into(),
        name: server.name.clone().into(),
        address: server.address.clone().into(),
        tailscale_ip: server.tailscale_ip.clone().map(Into::into),
        is_local: server.is_local,
        reachable,
    }
}
