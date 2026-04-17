use crate::diff_view::{fixture_diffs, DiffView, ReviewApply};
use crate::onboarding_view::{OnboardingCompleted, OnboardingView};
use crate::permission_prompt::{PermissionDecision, PermissionPrompt};
use crate::session_host::{SessionHost, SessionHostConfig, SessionStateChange};
use crate::settings_view::{SettingsDismissed, SettingsView};
use crate::sidebar::{SessionItem, SessionSelected, SettingsRequested, Sidebar};
use crate::sidecar_host::SidecarStatus;
use crate::tab_bar::{NewTabRequested, TabBar, TabCloseRequested, TabInfo, TabSelected};
use crate::theme::Theme;
use crate::timeline_view::{CheckpointDelete, CheckpointFork, CheckpointRestore, TimelineView};
use crate::workspace::{PromptSubmitted, WorkspaceView};
use chrono::Utc;
use cuartel_core::agent::{AgentType, HarnessRegistry};
use cuartel_core::credential_store::CredentialStore;
use cuartel_core::onboarding::OnboardingConfig;
use cuartel_core::review;
use cuartel_core::session::SessionState;
use cuartel_rivet::client::RivetClient;
use cuartel_terminal::TerminalView;
use gpui::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle;

const DEFAULT_AGENT: AgentType = AgentType::Pi;

struct SessionSlot {
    id: String,
    label: String,
    agent: AgentType,
    terminal: Entity<TerminalView>,
    diff_view: Entity<DiffView>,
    timeline_view: Entity<TimelineView>,
    permission_prompt: Entity<PermissionPrompt>,
    session_host: Entity<SessionHost>,
    state: SessionState,
    _host_sub: Subscription,
    _perm_sub: Subscription,
}

pub struct CuartelApp {
    sidebar: Entity<Sidebar>,
    workspace: Entity<WorkspaceView>,
    tab_bar: Entity<TabBar>,
    sessions: Vec<SessionSlot>,
    active_session_idx: usize,
    next_session_num: u32,
    sidecar_status: Arc<Mutex<SidecarStatus>>,
    rivet_client: Arc<Mutex<Option<RivetClient>>>,
    runtime_handle: Handle,
    sidecar_env: HashMap<String, String>,
    onboarding_view: Option<Entity<OnboardingView>>,
    settings_view: Option<Entity<SettingsView>>,
    registry: Arc<HarnessRegistry>,
    credentials: Arc<dyn CredentialStore>,
    onboarding_config: OnboardingConfig,
    data_dir: PathBuf,
    workspace_path: Option<PathBuf>,
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
        let sidebar = cx.new(|cx| Sidebar::new(sidecar_status.clone(), cx));

        let tab_bar = cx.new(|cx| TabBar::new(cx));

        let session_id = "session-1".to_string();
        let label = "Session 1".to_string();

        let terminal = cx.new(|cx| TerminalView::new_headless(cx));
        let permission_prompt = cx.new(|cx| PermissionPrompt::new(cx));
        let diff_view = cx.new(|cx| DiffView::new(fixture_diffs(), cx));
        let timeline_view = cx.new({
            let sid = session_id.clone();
            move |cx| TimelineView::new(sid, cx)
        });

        let workspace = cx.new({
            let tab_bar = tab_bar.clone();
            let permission_prompt = permission_prompt.clone();
            let terminal = terminal.clone();
            let diff_view = diff_view.clone();
            let timeline_view = timeline_view.clone();
            |cx| {
                WorkspaceView::new(
                    tab_bar,
                    terminal,
                    diff_view,
                    timeline_view,
                    permission_prompt,
                    cx,
                )
            }
        });

        let config = SessionHostConfig {
            session_id: session_id.clone(),
            agent_type: DEFAULT_AGENT.rivet_name().to_string(),
            actor_key: format!("cuartel-{session_id}"),
            workspace_id: "workspace-default".to_string(),
        };

        let session_host = cx.new({
            let runtime = runtime_handle.clone();
            let client = rivet_client.clone();
            let status = sidecar_status.clone();
            let terminal = terminal.clone();
            let perm = permission_prompt.clone();
            let env = sidecar_env.clone();
            move |cx| SessionHost::new(config, runtime, client, status, terminal, perm, env, cx)
        });

        let host_sub = cx.subscribe(&session_host, Self::on_session_state_change);
        let perm_sub = cx.subscribe(&permission_prompt, Self::on_permission_decision);

        let slot = SessionSlot {
            id: session_id.clone(),
            label: label.clone(),
            agent: DEFAULT_AGENT,
            terminal,
            diff_view,
            timeline_view,
            permission_prompt,
            session_host,
            state: SessionState::Created,
            _host_sub: host_sub,
            _perm_sub: perm_sub,
        };

        let tab_info = TabInfo {
            session_id: session_id.clone(),
            label: SharedString::from(label.clone()),
            agent: DEFAULT_AGENT,
            state: SessionState::Created,
        };
        tab_bar.update(cx, |tb, cx| {
            tb.set_tabs(vec![tab_info], cx);
            tb.set_active(&session_id, cx);
        });

        let sidebar_item = make_session_item(&slot);
        sidebar.update(cx, |sb, cx| {
            sb.set_sessions(vec![sidebar_item], cx);
        });

        cx.subscribe(&sidebar, Self::on_session_selected).detach();
        cx.subscribe(&sidebar, Self::on_settings_requested).detach();
        cx.subscribe(&workspace, Self::on_prompt_submitted).detach();
        cx.subscribe(&workspace, Self::on_review_apply).detach();
        cx.subscribe(&workspace, Self::on_checkpoint_restore).detach();
        cx.subscribe(&workspace, Self::on_checkpoint_fork).detach();
        cx.subscribe(&workspace, Self::on_checkpoint_delete).detach();
        cx.subscribe(&tab_bar, Self::on_tab_selected).detach();
        cx.subscribe(&tab_bar, Self::on_new_tab_requested).detach();
        cx.subscribe(&tab_bar, Self::on_tab_close_requested).detach();

        let onboarding_view = if !onboarding_config.completed {
            let initial_default = onboarding_config.default_harness.clone();
            let reg = registry.clone();
            let creds = credentials.clone();
            let ov =
                cx.new(move |cx| OnboardingView::new(reg, creds, initial_default, cx));
            cx.subscribe(&ov, Self::on_onboarding_completed).detach();
            Some(ov)
        } else {
            None
        };

        Self {
            sidebar,
            workspace,
            tab_bar,
            sessions: vec![slot],
            active_session_idx: 0,
            next_session_num: 2,
            sidecar_status,
            rivet_client,
            runtime_handle,
            sidecar_env,
            onboarding_view,
            settings_view: None,
            registry,
            credentials,
            onboarding_config,
            data_dir,
            workspace_path: None,
        }
    }

    fn active_slot(&self) -> &SessionSlot {
        &self.sessions[self.active_session_idx]
    }

    fn find_slot_idx(&self, session_id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == session_id)
    }

    fn switch_to(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.sessions.len() || idx == self.active_session_idx {
            return;
        }
        self.active_session_idx = idx;
        let slot = &self.sessions[idx];
        self.tab_bar.update(cx, |tb, cx| {
            tb.set_active(&slot.id, cx);
        });
        self.workspace.update(cx, |ws, cx| {
            ws.swap_views(
                slot.terminal.clone(),
                slot.diff_view.clone(),
                slot.timeline_view.clone(),
                slot.permission_prompt.clone(),
                cx,
            );
            ws.set_session_state(slot.state.clone(), cx);
        });
        cx.notify();
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        let num = self.next_session_num;
        self.next_session_num += 1;
        let session_id = format!("session-{num}");
        let label = format!("Session {num}");

        let terminal = cx.new(|cx| TerminalView::new_headless(cx));
        let permission_prompt = cx.new(|cx| PermissionPrompt::new(cx));
        let diff_view = cx.new(|cx| DiffView::new(fixture_diffs(), cx));
        let timeline_view = cx.new({
            let sid = session_id.clone();
            move |cx| TimelineView::new(sid, cx)
        });

        let config = SessionHostConfig {
            session_id: session_id.clone(),
            agent_type: DEFAULT_AGENT.rivet_name().to_string(),
            actor_key: format!("cuartel-{session_id}"),
            workspace_id: "workspace-default".to_string(),
        };

        let session_host = cx.new({
            let runtime = self.runtime_handle.clone();
            let client = self.rivet_client.clone();
            let status = self.sidecar_status.clone();
            let terminal = terminal.clone();
            let perm = permission_prompt.clone();
            let env = self.sidecar_env.clone();
            move |cx| SessionHost::new(config, runtime, client, status, terminal, perm, env, cx)
        });

        let host_sub = cx.subscribe(&session_host, Self::on_session_state_change);
        let perm_sub = cx.subscribe(&permission_prompt, Self::on_permission_decision);

        let slot = SessionSlot {
            id: session_id.clone(),
            label: label.clone(),
            agent: DEFAULT_AGENT,
            terminal,
            diff_view,
            timeline_view,
            permission_prompt,
            session_host,
            state: SessionState::Created,
            _host_sub: host_sub,
            _perm_sub: perm_sub,
        };

        self.sessions.push(slot);
        let new_idx = self.sessions.len() - 1;

        self.sync_tab_bar(cx);
        self.sync_sidebar(cx);
        self.switch_to(new_idx, cx);
    }

    fn close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.sessions.len() <= 1 {
            return;
        }
        let Some(idx) = self.find_slot_idx(session_id) else {
            return;
        };
        self.sessions.remove(idx);

        if self.active_session_idx >= self.sessions.len() {
            self.active_session_idx = self.sessions.len() - 1;
        } else if idx < self.active_session_idx {
            self.active_session_idx -= 1;
        } else if idx == self.active_session_idx {
            self.active_session_idx = self.active_session_idx.min(self.sessions.len() - 1);
        }

        self.sync_tab_bar(cx);
        self.sync_sidebar(cx);

        let slot = &self.sessions[self.active_session_idx];
        self.tab_bar.update(cx, |tb, cx| {
            tb.set_active(&slot.id, cx);
        });
        self.workspace.update(cx, |ws, cx| {
            ws.swap_views(
                slot.terminal.clone(),
                slot.diff_view.clone(),
                slot.timeline_view.clone(),
                slot.permission_prompt.clone(),
                cx,
            );
            ws.set_session_state(slot.state.clone(), cx);
        });
        cx.notify();
    }

    fn sync_tab_bar(&self, cx: &mut Context<Self>) {
        let tabs: Vec<TabInfo> = self
            .sessions
            .iter()
            .map(|s| TabInfo {
                session_id: s.id.clone(),
                label: SharedString::from(s.label.clone()),
                agent: s.agent.clone(),
                state: s.state.clone(),
            })
            .collect();
        self.tab_bar.update(cx, |tb, cx| tb.set_tabs(tabs, cx));
    }

    fn sync_sidebar(&self, cx: &mut Context<Self>) {
        let items: Vec<SessionItem> = self.sessions.iter().map(make_session_item).collect();
        self.sidebar.update(cx, |sb, cx| sb.set_sessions(items, cx));
    }

    // --- Event handlers ---

    fn on_session_state_change(
        &mut self,
        _host: Entity<SessionHost>,
        event: &SessionStateChange,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.find_slot_idx(&event.session_id) else {
            return;
        };
        self.sessions[idx].state = event.state.clone();

        self.tab_bar.update(cx, |tb, cx| {
            tb.update_tab_state(&event.session_id, event.state.clone(), cx);
        });
        self.sync_sidebar(cx);

        if idx == self.active_session_idx {
            self.workspace.update(cx, |ws, cx| {
                ws.set_session_state(event.state.clone(), cx);
            });
        }
    }

    fn on_prompt_submitted(
        &mut self,
        _view: Entity<WorkspaceView>,
        event: &PromptSubmitted,
        cx: &mut Context<Self>,
    ) {
        let slot = &self.sessions[self.active_session_idx];
        slot.session_host.update(cx, |host, _| {
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
            "onboarding completed: default_harness={:?}",
            self.onboarding_config.default_harness,
        );
        self.onboarding_view = None;
        cx.notify();
    }

    fn on_settings_requested(
        &mut self,
        _sidebar: Entity<Sidebar>,
        _event: &SettingsRequested,
        cx: &mut Context<Self>,
    ) {
        let registry = self.registry.clone();
        let credentials = self.credentials.clone();
        let current_default = self.onboarding_config.default_harness.clone();
        let sv = cx.new(move |cx| SettingsView::new(registry, credentials, current_default, cx));
        cx.subscribe(&sv, Self::on_settings_dismissed).detach();
        self.settings_view = Some(sv);
        cx.notify();
    }

    fn on_settings_dismissed(
        &mut self,
        _view: Entity<SettingsView>,
        event: &SettingsDismissed,
        cx: &mut Context<Self>,
    ) {
        if event.default_harness != self.onboarding_config.default_harness {
            self.onboarding_config.default_harness = event.default_harness.clone();
            if let Err(e) = self.onboarding_config.save(&self.data_dir) {
                log::warn!("failed to persist settings change: {e}");
            }
            log::info!(
                "settings: default_harness changed to {:?}",
                self.onboarding_config.default_harness,
            );
        }
        self.settings_view = None;
        cx.notify();
    }

    fn on_session_selected(
        &mut self,
        _sidebar: Entity<Sidebar>,
        event: &SessionSelected,
        cx: &mut Context<Self>,
    ) {
        if let Some(idx) = self.find_slot_idx(&event.id) {
            self.switch_to(idx, cx);
        }
    }

    fn on_tab_selected(
        &mut self,
        _tab_bar: Entity<TabBar>,
        event: &TabSelected,
        cx: &mut Context<Self>,
    ) {
        if let Some(idx) = self.find_slot_idx(&event.session_id) {
            self.switch_to(idx, cx);
        }
    }

    fn on_new_tab_requested(
        &mut self,
        _tab_bar: Entity<TabBar>,
        _event: &NewTabRequested,
        cx: &mut Context<Self>,
    ) {
        self.create_session(cx);
    }

    fn on_tab_close_requested(
        &mut self,
        _tab_bar: Entity<TabBar>,
        event: &TabCloseRequested,
        cx: &mut Context<Self>,
    ) {
        self.close_session(&event.session_id.clone(), cx);
    }

    fn on_permission_decision(
        &mut self,
        _prompt: Entity<PermissionPrompt>,
        event: &PermissionDecision,
        cx: &mut Context<Self>,
    ) {
        let (id, session_id, approve) = match event {
            PermissionDecision::Approve { id, session_id } => {
                (id.clone(), session_id.clone(), true)
            }
            PermissionDecision::Deny { id, session_id } => {
                (id.clone(), session_id.clone(), false)
            }
        };
        // Route to the correct session's host by matching on permission's session_id.
        // Fall back to the active session if no match (e.g. fixture data with no real session_id).
        let slot = self
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .unwrap_or_else(|| self.active_slot());
        slot.session_host.update(cx, |host, _cx| {
            host.decide(id, approve);
        });
    }

    fn on_review_apply(
        &mut self,
        _view: Entity<WorkspaceView>,
        event: &ReviewApply,
        _cx: &mut Context<Self>,
    ) {
        let host_root = match &self.workspace_path {
            Some(p) => p.clone(),
            None => {
                log::warn!("[review] no workspace path set — cannot apply review");
                return;
            }
        };
        let slot = self.active_slot();
        let diffs: Vec<_> = slot.diff_view.read(_cx).diffs().to_vec();
        let decisions = event.decisions.clone();
        match review::plan_review(&diffs, &decisions, &host_root) {
            Ok(plan) => match review::execute_review(&plan, &host_root) {
                Ok(report) => {
                    log::info!(
                        "[review] applied: {} written, {} deleted, {} skipped",
                        report.files_written,
                        report.files_deleted,
                        report.files_skipped,
                    );
                }
                Err(e) => log::error!("[review] execute failed: {e}"),
            },
            Err(e) => log::error!("[review] plan failed: {e}"),
        }
    }

    fn on_checkpoint_restore(
        &mut self,
        _view: Entity<WorkspaceView>,
        event: &CheckpointRestore,
        _cx: &mut Context<Self>,
    ) {
        // TODO: Phase 6d integration — call rivet_client.restore_checkpoint
        // with fork=false to rewind the active session in place. For now,
        // log the intent so the UI flow is exercisable.
        log::info!(
            "[checkpoint] restore requested: checkpoint_id={}",
            event.checkpoint_id,
        );
    }

    fn on_checkpoint_fork(
        &mut self,
        _view: Entity<WorkspaceView>,
        event: &CheckpointFork,
        cx: &mut Context<Self>,
    ) {
        log::info!(
            "[checkpoint] fork requested: checkpoint_id={} session_id={}",
            event.checkpoint_id,
            event.session_id,
        );
        // Create a new session as the forked branch. The Rivet
        // restore_checkpoint(fork=true) call will be wired in a follow-up
        // once the sidecar supports it. For now, create the session slot
        // so the tab/sidebar UX is complete.
        let num = self.next_session_num;
        self.next_session_num += 1;
        let session_id = format!("session-{num}");
        let label = format!("Fork {num} (from {})", &event.checkpoint_id[..8.min(event.checkpoint_id.len())]);

        let terminal = cx.new(|cx| TerminalView::new_headless(cx));
        let permission_prompt = cx.new(|cx| PermissionPrompt::new(cx));
        let diff_view = cx.new(|cx| DiffView::new(vec![], cx));
        let timeline_view = cx.new({
            let sid = session_id.clone();
            move |cx| TimelineView::new(sid, cx)
        });

        let config = SessionHostConfig {
            session_id: session_id.clone(),
            agent_type: DEFAULT_AGENT.rivet_name().to_string(),
            actor_key: format!("cuartel-{session_id}"),
            workspace_id: "workspace-default".to_string(),
        };

        let session_host = cx.new({
            let runtime = self.runtime_handle.clone();
            let client = self.rivet_client.clone();
            let status = self.sidecar_status.clone();
            let terminal = terminal.clone();
            let perm = permission_prompt.clone();
            let env = self.sidecar_env.clone();
            move |cx| SessionHost::new(config, runtime, client, status, terminal, perm, env, cx)
        });

        let host_sub = cx.subscribe(&session_host, Self::on_session_state_change);
        let perm_sub = cx.subscribe(&permission_prompt, Self::on_permission_decision);

        let slot = SessionSlot {
            id: session_id.clone(),
            label,
            agent: DEFAULT_AGENT,
            terminal,
            diff_view,
            timeline_view,
            permission_prompt,
            session_host,
            state: SessionState::Created,
            _host_sub: host_sub,
            _perm_sub: perm_sub,
        };

        self.sessions.push(slot);
        let new_idx = self.sessions.len() - 1;

        self.sync_tab_bar(cx);
        self.sync_sidebar(cx);
        self.switch_to(new_idx, cx);
    }

    fn on_checkpoint_delete(
        &mut self,
        _view: Entity<WorkspaceView>,
        event: &CheckpointDelete,
        _cx: &mut Context<Self>,
    ) {
        // TODO: Phase 6d integration — call rivet_client.delete_checkpoint
        // and remove from the local checkpoint store. For now, log the intent.
        log::info!(
            "[checkpoint] delete requested: checkpoint_id={}",
            event.checkpoint_id,
        );
    }

    pub fn set_workspace_path(&mut self, path: PathBuf) {
        self.workspace_path = Some(path);
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
            .children(self.settings_view.clone())
    }
}

fn make_session_item(slot: &SessionSlot) -> SessionItem {
    SessionItem::new(&slot.id, &slot.label, slot.agent.clone(), slot.state.clone())
        .with_created_at(Utc::now())
}
