use cuartel_rivet::client::{GetOrCreateRequest, RivetClient};
use cuartel_rivet::sidecar::Sidecar;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::{Handle, Runtime};

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
    client: Arc<Mutex<Option<RivetClient>>>,
    handle: Handle,
}

/// Build the shared tokio runtime and leak it so worker threads stay alive
/// for the entire app lifetime. The returned `Handle` is passed to
/// `SidecarHost::spawn` and `GatewayHost::spawn` (and anything else that
/// needs to schedule async work) so the process runs on a single pool.
pub fn build_shared_runtime() -> Handle {
    let rt: Runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("cuartel-tokio")
        .build()
        .expect("failed to build tokio runtime");
    let handle = rt.handle().clone();
    // Leak: the multi-threaded runtime owns its worker threads; dropping
    // the `Runtime` would shut them down. We want them alive for the
    // whole process, so we deliberately forget the value.
    Box::leak(Box::new(rt));
    handle
}

impl SidecarHost {
    /// Spawn the rivet sidecar on the provided tokio `Handle`.
    ///
    /// `env` is forwarded verbatim to `Command::new("npx").env(...)` before
    /// the child is started (task 3l). Typical usage: pass the credentials
    /// for the default harness (e.g. `ANTHROPIC_API_KEY`) so the rivetkit
    /// server — and by extension every agent-os subprocess it spawns —
    /// inherits them. Environment changes after spawn are not respected;
    /// restart the sidecar to pick up new vars.
    pub fn spawn(
        handle: Handle,
        rivet_dir: PathBuf,
        port: u16,
        env: HashMap<String, String>,
    ) -> Self {
        let status = Arc::new(Mutex::new(SidecarStatus::Idle));
        let client = Arc::new(Mutex::new(None));

        let status_bg = status.clone();
        let client_bg = client.clone();
        handle.spawn(async move {
            let mut sidecar = Sidecar::new(rivet_dir, port);
            sidecar.set_env(env);

            *status_bg.lock() = SidecarStatus::Installing;
            if let Err(e) = sidecar.ensure_deps_installed().await {
                log::error!("rivet deps install failed: {e}");
                *status_bg.lock() = SidecarStatus::Failed(format!("npm install: {e}"));
                // Hold the Sidecar alive on this task so child-process
                // handles aren't dropped mid-install-retry from a user's
                // future action.
                std::future::pending::<()>().await;
                return;
            }

            *status_bg.lock() = SidecarStatus::Starting;
            if let Err(e) = sidecar.start().await {
                log::error!("rivet sidecar start failed: {e}");
                *status_bg.lock() = SidecarStatus::Failed(format!("start: {e}"));
                std::future::pending::<()>().await;
                return;
            }

            let rivet_client = RivetClient::new(&format!("http://localhost:{}", port));
            *client_bg.lock() = Some(rivet_client.clone());
            *status_bg.lock() = SidecarStatus::Ready;

            smoke_test(&rivet_client).await;

            // Keep the Sidecar (and therefore the Node.js child process)
            // owned by this task for the lifetime of the runtime.
            std::future::pending::<()>().await;
        });

        Self {
            status,
            client,
            handle,
        }
    }

    pub fn status(&self) -> Arc<Mutex<SidecarStatus>> {
        self.status.clone()
    }

    pub fn client(&self) -> Arc<Mutex<Option<RivetClient>>> {
        self.client.clone()
    }

    /// Handle to the shared tokio runtime. Callers may `.spawn(future)` to
    /// run additional async work on this runtime — notably, SessionHost
    /// uses it to drive Rivet client calls and the event stream.
    pub fn runtime_handle(&self) -> Handle {
        self.handle.clone()
    }
}

async fn smoke_test(client: &RivetClient) {
    match client.health().await {
        Ok(h) => log::info!(
            "rivet health: status={} runtime={} version={}",
            h.status,
            h.runtime,
            h.version,
        ),
        Err(e) => {
            log::warn!("rivet /health failed: {e}");
            return;
        }
    }

    match client.list_actor_names("default").await {
        Ok(names) => {
            let registered: Vec<&String> = names.names.keys().collect();
            log::info!("rivet registered actors (default ns): {:?}", registered);
        }
        Err(e) => log::warn!("rivet /actors/names failed: {e}"),
    }

    // Exercise the idempotent get-or-create path for our `vm` actor.
    let req = GetOrCreateRequest {
        name: "vm",
        key: "cuartel-main",
        runner_name_selector: "default",
        crash_policy: "kill",
    };
    match client.get_or_create_actor(&req).await {
        Ok(res) => log::info!(
            "rivet actor vm/{}: id={} created={}",
            req.key,
            res.actor.actor_id,
            res.created,
        ),
        Err(e) => log::warn!("rivet PUT /actors failed: {e}"),
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
