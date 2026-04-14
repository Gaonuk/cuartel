use cuartel_rivet::client::RivetClient;
use cuartel_rivet::sidecar::Sidecar;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarStatus {
    Idle,
    Installing,
    Starting,
    Ready,
    Failed(String),
}

pub struct SidecarHost {
    status: Arc<Mutex<SidecarStatus>>,
    #[allow(dead_code)]
    client: Arc<Mutex<Option<RivetClient>>>,
}

impl SidecarHost {
    pub fn spawn(rivet_dir: PathBuf, port: u16) -> Self {
        let status = Arc::new(Mutex::new(SidecarStatus::Idle));
        let client = Arc::new(Mutex::new(None));

        let status_bg = status.clone();
        let client_bg = client.clone();
        thread::Builder::new()
            .name("cuartel-sidecar".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        *status_bg.lock() = SidecarStatus::Failed(format!("runtime: {e}"));
                        return;
                    }
                };

                rt.block_on(async move {
                    let mut sidecar = Sidecar::new(rivet_dir, port);

                    *status_bg.lock() = SidecarStatus::Installing;
                    if let Err(e) = sidecar.ensure_deps_installed().await {
                        log::error!("rivet deps install failed: {e}");
                        *status_bg.lock() = SidecarStatus::Failed(format!("npm install: {e}"));
                        return;
                    }

                    *status_bg.lock() = SidecarStatus::Starting;
                    if let Err(e) = sidecar.start().await {
                        log::error!("rivet sidecar start failed: {e}");
                        *status_bg.lock() = SidecarStatus::Failed(format!("start: {e}"));
                        return;
                    }

                    *client_bg.lock() =
                        Some(RivetClient::new(&format!("http://localhost:{}", port)));
                    *status_bg.lock() = SidecarStatus::Ready;

                    // Keep the runtime alive so the child process isn't reaped.
                    std::future::pending::<()>().await;
                });
            })
            .expect("failed to spawn sidecar thread");

        Self { status, client }
    }

    pub fn status(&self) -> Arc<Mutex<SidecarStatus>> {
        self.status.clone()
    }
}

/// Resolve the workspace `rivet/` directory. Looks upward from the app crate
/// at build time (dev) and falls back to `./rivet` next to the executable.
pub fn default_rivet_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(ws) = manifest.parent().and_then(|p| p.parent()) {
        let candidate = ws.join("rivet");
        if candidate.exists() {
            return candidate;
        }
    }
    std::env::current_dir()
        .map(|d| d.join("rivet"))
        .unwrap_or_else(|_| PathBuf::from("rivet"))
}
