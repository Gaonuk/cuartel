mod app;
mod assets;
mod diff_view;
mod onboarding_view;
mod permission_prompt;
mod server_registry_host;
mod session_host;
mod settings_view;
mod sidebar;
mod sidebar_visuals;
mod sidecar_host;
mod tab_bar;
mod theme;
mod timeline_view;
mod workspace;

use app::CuartelApp;
use assets::Assets;
use cuartel_core::agent::HarnessRegistry;
use cuartel_core::config::AppConfig;
use cuartel_core::credential_store::{
    env_for_harness, CredentialStore, KeychainCredentialStore, MemoryCredentialStore,
};
use cuartel_core::onboarding::OnboardingConfig;
use cuartel_db::Database;
use cuartel_remote::{local_base_url, ServerRegistry, TailscaleClient};
use gpui::*;
use sidecar_host::{default_rivet_dir, SidecarHost};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

const RIVET_PORT: u16 = 6420;

fn main() {
    env_logger::init();

    let app_config = AppConfig::default();
    let data_dir = app_config.data_dir.clone();

    let registry = Arc::new(HarnessRegistry::with_builtins());
    let credentials: Arc<dyn CredentialStore> = build_credential_store();
    let onboarding = OnboardingConfig::load(&data_dir).unwrap_or_else(|e| {
        log::warn!("failed to load onboarding config: {e}; falling back to defaults");
        OnboardingConfig::default()
    });

    // Task 3l: assemble env vars for the default harness (if any) so the
    // rivetkit server — and the agent-os subprocess it spawns — inherit
    // the credentials the user configured in onboarding. Without this,
    // Pi/Claude/etc. crash with "Agent process exited" because their
    // required API keys are absent from the child env.
    let sidecar_env: HashMap<String, String> = match onboarding.default_harness.as_ref() {
        Some(agent) => env_for_harness(&registry, credentials.as_ref(), agent),
        None => HashMap::new(),
    };
    if sidecar_env.is_empty() {
        log::info!(
            "no credentials available for default harness ({:?}); \
             sidecar will start without injected env — configure harness \
             in onboarding to enable agent spawn",
            onboarding.default_harness,
        );
    }

    // Leak so the sidecar host lives for the entire app lifetime without
    // requiring a Send + 'static move into GPUI's run closure.
    let sidecar: &'static SidecarHost = Box::leak(Box::new(SidecarHost::spawn(
        default_rivet_dir(),
        RIVET_PORT,
        sidecar_env.clone(),
    )));

    // Phase 7: persistent server registry backed by the same SQLite DB the
    // rest of the app will eventually share. Falls back to "no registry" on
    // IO failure so the app still boots — the sidebar will render the live
    // sidecar status like it did pre-phase-7.
    let server_registry = match build_server_registry(&data_dir, RIVET_PORT) {
        Ok(reg) => Some(Arc::new(reg)),
        Err(e) => {
            log::warn!("server registry unavailable ({e}); continuing without it");
            None
        }
    };

    let registry_for_app = registry.clone();
    let credentials_for_app = credentials.clone();
    let onboarding_for_app = onboarding.clone();
    let data_dir_for_app = data_dir.clone();
    let env_for_app = sidecar_env;
    let server_registry_for_app = server_registry.clone();

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if let Err(e) = Assets.load_fonts(cx) {
                log::error!("failed to load fonts: {e}");
            }

            let status_handle = sidecar.status();
            let client_handle = sidecar.client();
            let runtime_handle = sidecar.runtime_handle();
            let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
            let registry = registry_for_app.clone();
            let credentials = credentials_for_app.clone();
            let onboarding = onboarding_for_app.clone();
            let data_dir = data_dir_for_app.clone();
            let server_registry = server_registry_for_app.clone();
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Cuartel".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| {
                        CuartelApp::new(
                            status_handle,
                            client_handle,
                            runtime_handle,
                            registry,
                            credentials,
                            onboarding,
                            data_dir,
                            env_for_app,
                            server_registry,
                            cx,
                        )
                    })
                },
            )
            .unwrap();
        });
}

/// Open the shared SQLite DB and seed the `local` row so the sidebar has
/// something to render on the very first launch.
fn build_server_registry(
    data_dir: &std::path::Path,
    rivet_port: u16,
) -> anyhow::Result<ServerRegistry> {
    let db_path = data_dir.join("cuartel.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = Database::open(&db_path)?;
    let db = Arc::new(StdMutex::new(db));
    let tailscale = Arc::new(TailscaleClient::new());
    let registry = ServerRegistry::new(db, tailscale);
    registry.ensure_local(&local_base_url(rivet_port))?;
    Ok(registry)
}

/// Build a credential store. Prefers the system keychain (task 3k) but
/// falls back to an in-memory store when the keychain is unreachable — we
/// never want the app to refuse to start because the user is on a headless
/// machine or the keychain plugin is locked.
fn build_credential_store() -> Arc<dyn CredentialStore> {
    let keychain = KeychainCredentialStore::new();
    // Round-trip a sentinel read to detect "no keyring available" on this
    // host. keyring::Error::NoEntry is the happy-path signal (entry is
    // empty). Anything else means we couldn't even talk to the backend.
    match keychain.get("__cuartel_probe__", "__ping__") {
        Ok(_) => {
            log::info!("credential store: macOS Keychain");
            Arc::new(keychain)
        }
        Err(e) => {
            log::warn!(
                "credential store: keychain unavailable ({e}); falling back to in-memory"
            );
            Arc::new(MemoryCredentialStore::new())
        }
    }
}
