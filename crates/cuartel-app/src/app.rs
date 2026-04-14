use crate::sidebar::{SessionItem, SessionSelected, Sidebar};
use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;
use crate::workspace::WorkspaceView;
use chrono::{Duration as ChronoDuration, Utc};
use cuartel_core::agent::AgentType;
use cuartel_core::session::SessionState;
use gpui::*;
use parking_lot::Mutex;
use std::sync::Arc;

pub struct CuartelApp {
    sidebar: Entity<Sidebar>,
    workspace: Entity<WorkspaceView>,
}

impl CuartelApp {
    pub fn new(
        sidecar_status: Arc<Mutex<SidecarStatus>>,
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

        let sidebar = cx.new(|cx| {
            let mut sb = Sidebar::new(sidecar_status, cx);
            sb.set_sessions(fixtures, cx);
            sb
        });

        let workspace = cx.new(|cx| WorkspaceView::new(initial_label, initial_agent, cx));

        cx.subscribe(&sidebar, Self::on_session_selected).detach();

        Self { sidebar, workspace }
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
}

impl Render for CuartelApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        div()
            .id("cuartel-root")
            .flex()
            .size_full()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(self.sidebar.clone())
            .child(self.workspace.clone())
    }
}

/// Fixture sessions exercising every `SessionState` variant the sidebar can
/// render. Kept here until 3f lands real Rivet-backed sessions.
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
