use crate::diff_view::{fixture_diffs, DiffView};
use crate::onboarding_view::{OnboardingCompleted, OnboardingView};
use crate::permission_prompt::{PermissionDecision, PermissionPrompt};
use crate::session_host::{SessionHost, SessionStateChange};
use crate::sidebar::{SessionItem, SessionSelected, Sidebar};
use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;
use crate::workspace::{PromptSubmitted, WorkspaceView};
use chrono::Utc;
use cuartel_core::agent::{AgentType, HarnessRegistry};
use cuartel_core::credential_store::CredentialStore;
use cuartel_core::onboarding::OnboardingConfig;
use cuartel_core::session::SessionState;
use cuartel_rivet::client::RivetClient;
use cuartel_terminal::TerminalView;
use gpui::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle;

const SESSION_LABEL: &str = "cuartel-main";
const SESSION_AGENT: AgentType = AgentType::Pi;

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
        let initial_state = SessionState::Created;
        let initial_session = make_session_item(initial_state.clone());

        let sidebar = cx.new(|cx| {
            let mut sb = Sidebar::new(sidecar_status.clone(), cx);
            sb.set_sessions(vec![initial_session], cx);
            sb
        });

        let terminal = cx.new(|cx| TerminalView::new_headless(cx));

        let permission_prompt = cx.new(|cx| PermissionPrompt::new(cx));

        // 4c: review panel mounts against fixture diffs until 4f wires it
        // to a live overlay snapshot from the running session.
        let diff_view = cx.new(|cx| DiffView::new(fixture_diffs(), cx));

        let workspace = cx.new({
            let permission_prompt = permission_prompt.clone();
            let terminal = terminal.clone();
            let diff_view = diff_view.clone();
            |cx| {
                WorkspaceView::new(
                    SharedString::from(SESSION_LABEL),
                    SharedString::from(SESSION_AGENT.display_name()),
                    terminal,
                    diff_view,
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
        cx.subscribe(&session_host, Self::on_session_state_change)
            .detach();
        cx.subscribe(&workspace, Self::on_prompt_submitted).detach();
        cx.subscribe(&permission_prompt, Self::on_permission_decision)
            .detach();

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

    fn on_session_state_change(
        &mut self,
        _host: Entity<SessionHost>,
        event: &SessionStateChange,
        cx: &mut Context<Self>,
    ) {
        let item = make_session_item(event.state.clone());
        self.sidebar.update(cx, |sb, cx| {
            sb.set_sessions(vec![item], cx);
        });
        self.workspace.update(cx, |ws, cx| {
            ws.set_session_state(event.state.clone(), cx);
        });
    }

    fn on_prompt_submitted(
        &mut self,
        _view: Entity<WorkspaceView>,
        event: &PromptSubmitted,
        cx: &mut Context<Self>,
    ) {
        self.session_host.update(cx, |host, _| {
            host.send_prompt(event.text.clone());
        });
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
        _event: &SessionSelected,
        _cx: &mut Context<Self>,
    ) {
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

fn make_session_item(state: SessionState) -> SessionItem {
    SessionItem::new("cuartel-main", SESSION_LABEL, SESSION_AGENT, state)
        .with_created_at(Utc::now())
}
