use crate::onboarding_view::{OnboardingCompleted, OnboardingView};
use crate::permission_prompt::{PendingPermission, PermissionDecision, PermissionPrompt};
use crate::session_host::SessionHost;
use crate::sidebar::{SessionItem, SessionSelected, Sidebar};
use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;
use crate::workspace::WorkspaceView;
use chrono::{Duration as ChronoDuration, Utc};
use cuartel_core::agent::{AgentType, HarnessRegistry};
use cuartel_core::credential_store::CredentialStore;
use cuartel_core::onboarding::OnboardingConfig;
use cuartel_core::session::SessionState;
use cuartel_rivet::client::RivetClient;
use cuartel_terminal::TerminalView;
use gpui::*;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle;

pub struct CuartelApp {
    sidebar: Entity<Sidebar>,
    workspace: Entity<WorkspaceView>,
    #[allow(dead_code)]
    permission_prompt: Entity<PermissionPrompt>,
    session_host: Entity<SessionHost>,
    onboarding_view: Option<Entity<OnboardingView>>,
    onboarding_config: OnboardingConfig,
    data_dir: PathBuf,
}

impl CuartelApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sidecar_status: Arc<Mutex<SidecarStatus>>,
        rivet_client: Arc<Mutex<Option<RivetClient>>>,
        runtime_handle: Handle,
        registry: Arc<HarnessRegistry>,
        credentials: Arc<dyn CredentialStore>,
        onboarding_config: OnboardingConfig,
        data_dir: PathBuf,
        sidecar_env: HashMap<String, String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let fixtures = fixture_sessions();
        let initial_label = fixtures
            .first()
            .map(|s| s.label.clone())
            .unwrap_or_else(|| SharedString::from("(no sessions)"));
        let initial_agent = fixtures
            .first()
            .map(|s| SharedString::from(s.agent.display_name().to_string()))
            .unwrap_or_else(|| SharedString::from(""));
        let initial_session_id = fixtures
            .first()
            .map(|s| s.id.to_string())
            .unwrap_or_default();
        let initial_session_label = initial_label.clone();

        let sidebar = cx.new(|cx| {
            let mut sb = Sidebar::new(sidecar_status.clone(), cx);
            sb.set_sessions(fixtures, cx);
            sb
        });

        // Single headless terminal shared between the workspace (for
        // rendering) and the session host (for feeding remote agent output).
        let terminal = cx.new(|cx| TerminalView::new_headless(cx));

        let permission_prompt = cx.new(|cx| {
            let mut pp = PermissionPrompt::new(cx);
            for pending in fixture_permissions(&initial_session_id, &initial_session_label) {
                pp.enqueue(pending, cx);
            }
            pp
        });

        let workspace = cx.new({
            let permission_prompt = permission_prompt.clone();
            let terminal = terminal.clone();
            |cx| {
                WorkspaceView::new(
                    initial_label,
                    initial_agent,
                    terminal,
                    permission_prompt,
                    cx,
                )
            }
        });

        let session_host = cx.new({
            let terminal = terminal.clone();
            let permission_prompt = permission_prompt.clone();
            move |cx| {
                SessionHost::new(
                    runtime_handle,
                    rivet_client,
                    sidecar_status,
                    terminal,
                    permission_prompt,
                    sidecar_env,
                    cx,
                )
            }
        });

        cx.subscribe(&sidebar, Self::on_session_selected).detach();
        cx.subscribe(&permission_prompt, Self::on_permission_decision)
            .detach();

        // Only show the onboarding modal on first run (until the user
        // clicks "Save and continue"). After that it's dismissed for the
        // rest of the session; surfacing it from settings is a Phase 3
        // follow-up.
        let onboarding_view = if !onboarding_config.completed {
            let initial_default = onboarding_config.default_harness.clone();
            let ov =
                cx.new(move |cx| OnboardingView::new(registry, credentials, initial_default, cx));
            cx.subscribe(&ov, Self::on_onboarding_completed).detach();
            Some(ov)
        } else {
            None
        };

        Self {
            sidebar,
            workspace,
            permission_prompt,
            session_host,
            onboarding_view,
            onboarding_config,
            data_dir,
        }
    }

    fn on_onboarding_completed(
        &mut self,
        _view: Entity<OnboardingView>,
        event: &OnboardingCompleted,
        cx: &mut Context<Self>,
    ) {
        self.onboarding_config.default_harness = Some(event.default_harness.clone());
        self.onboarding_config.completed = true;
        if let Err(e) = self.onboarding_config.save(&self.data_dir) {
            log::warn!("failed to persist onboarding config: {e}");
        }
        log::info!(
            "onboarding completed: default_harness={:?} (restart cuartel to pick up \
             sidecar env injection)",
            self.onboarding_config.default_harness,
        );
        self.onboarding_view = None;
        cx.notify();
    }

    fn on_session_selected(
        &mut self,
        _sidebar: Entity<Sidebar>,
        event: &SessionSelected,
        cx: &mut Context<Self>,
    ) {
        let label = event.label.clone();
        let agent = event.agent.clone();
        self.workspace.update(cx, |ws, cx| {
            ws.set_active_session(label, agent, cx);
        });
    }

    fn on_permission_decision(
        &mut self,
        _prompt: Entity<PermissionPrompt>,
        event: &PermissionDecision,
        cx: &mut Context<Self>,
    ) {
        let (id, approve) = match event {
            PermissionDecision::Approve { id, .. } => (id.clone(), true),
            PermissionDecision::Deny { id, .. } => (id.clone(), false),
        };
        self.session_host.update(cx, |host, _cx| {
            host.decide(id, approve);
        });
    }
}

impl Render for CuartelApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        div()
            .id("cuartel-root")
            .relative()
            .flex()
            .size_full()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(self.sidebar.clone())
            .child(self.workspace.clone())
            .children(self.onboarding_view.clone())
    }
}

/// Fixture sessions exercising every `SessionState` variant the sidebar can
/// render. Real Rivet-backed sessions are driven by `SessionHost`; the
/// sidebar fixtures stay until 3d evolves to list those instead.
fn fixture_sessions() -> Vec<SessionItem> {
    let now = Utc::now();
    vec![
        SessionItem::new(
            "sess-fix-auth",
            "fix-auth-bug",
            AgentType::Pi,
            SessionState::Running,
        )
        .with_created_at(now - ChronoDuration::minutes(2)),
        SessionItem::new(
            "sess-dark-mode",
            "add-dark-mode",
            AgentType::ClaudeCode,
            SessionState::Ready,
        )
        .with_created_at(now - ChronoDuration::minutes(12)),
        SessionItem::new(
            "sess-refactor",
            "refactor-orm",
            AgentType::Codex,
            SessionState::Booting,
        )
        .with_created_at(now - ChronoDuration::seconds(8)),
        SessionItem::new(
            "sess-tests",
            "flaky-test-hunt",
            AgentType::OpenCode,
            SessionState::Paused,
        )
        .with_created_at(now - ChronoDuration::hours(1)),
        SessionItem::new(
            "sess-migration",
            "db-migration-0042",
            AgentType::Pi,
            SessionState::Reviewing,
        )
        .with_created_at(now - ChronoDuration::minutes(35)),
        SessionItem::new(
            "sess-crash",
            "rate-limit-retry",
            AgentType::ClaudeCode,
            SessionState::Error("anthropic 429 timeout".into()),
        )
        .with_created_at(now - ChronoDuration::minutes(47)),
        SessionItem::new(
            "sess-snapshot",
            "pre-deploy-checkpoint",
            AgentType::Pi,
            SessionState::Checkpointed,
        )
        .with_created_at(now - ChronoDuration::hours(3)),
    ]
}

/// Fixture permission requests shown above the terminal so the approve/deny
/// flow is exercisable without waiting for Pi to actually emit a
/// `request/permission`. Real permission events from `SessionHost` are
/// enqueued into the same `PermissionPrompt` entity and appended to the
/// queue.
fn fixture_permissions(session_id: &str, session_label: &SharedString) -> Vec<PendingPermission> {
    vec![
        PendingPermission::new(
            "perm-001",
            session_id,
            session_label.clone(),
            "bash",
            json!({
                "command": "cargo test -p cuartel-core --lib session::tests::happy_path_create_boot_prompt_complete",
                "cwd": "/workspace",
                "timeout_ms": 60000
            }),
        ),
        PendingPermission::new(
            "perm-002",
            session_id,
            session_label.clone(),
            "write_file",
            json!({
                "path": "/workspace/src/auth/middleware.rs",
                "contents_preview": "pub fn verify_jwt(token: &str) -> Result<Claims, AuthError> {\n    // ...\n}",
                "byte_len": 1248
            }),
        ),
        PendingPermission::new(
            "perm-003",
            session_id,
            session_label.clone(),
            "fetch",
            json!({
                "url": "https://api.github.com/repos/anthropics/anthropic-sdk-python/issues",
                "method": "GET",
                "headers": { "Accept": "application/vnd.github+json" }
            }),
        ),
    ]
}
