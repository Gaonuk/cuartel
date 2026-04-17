pub mod registry;
pub mod server;
pub mod sync;
pub mod tailscale;

pub use registry::{local_base_url, rivet_client_for, ServerRegistry, DEFAULT_RIVET_PORT};
pub use server::RemoteServer;
pub use sync::{SessionSnapshot, SessionSyncService, SyncDirection, SyncRequest, SyncResult};
pub use tailscale::{TailnetSnapshot, TailnetStatus, TailscaleClient, TailscaleDevice};
