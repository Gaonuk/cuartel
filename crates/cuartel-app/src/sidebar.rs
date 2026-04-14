use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;
use chrono::{DateTime, Utc};
use cuartel_core::agent::AgentType;
use cuartel_core::session::SessionState;
use gpui::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

/// Event fired when the user picks a session in the sidebar. The parent
/// (`CuartelApp`) subscribes and forwards it to the workspace view.
#[derive(Clone, Debug)]
#[allow(dead_code)] // `id` wired for 3f when clicks drive real Rivet sessions.
pub struct SessionSelected {
    pub id: String,
    pub label: SharedString,
    pub agent: SharedString,
}

pub struct Sidebar {
    sessions: Vec<SessionItem>,
    selected_index: Option<usize>,
    sidecar_status: Arc<Mutex<SidecarStatus>>,
    last_seen_status: SidecarStatus,
    _poll_task: Task<()>,
}

impl EventEmitter<SessionSelected> for Sidebar {}

#[derive(Clone)]
pub struct SessionItem {
    pub id: SharedString,
    pub label: SharedString,
    pub agent: AgentType,
    pub state: SessionState,
    pub created_at: DateTime<Utc>,
}

impl SessionItem {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        agent: AgentType,
        state: SessionState,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            agent,
            state,
            created_at: Utc::now(),
        }
    }

    pub fn with_created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = at;
        self
    }
}

impl Sidebar {
    pub fn new(
        sidecar_status: Arc<Mutex<SidecarStatus>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let status_poll = sidecar_status.clone();
        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let current = status_poll.lock().clone();
                if this
                    .update(cx, |sidebar, cx| {
                        if sidebar.last_seen_status != current {
                            sidebar.last_seen_status = current;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let initial = sidecar_status.lock().clone();
        Self {
            sessions: vec![],
            selected_index: None,
            last_seen_status: initial,
            sidecar_status,
            _poll_task: poll_task,
        }
    }

    pub fn set_sessions(&mut self, items: Vec<SessionItem>, cx: &mut Context<Self>) {
        self.sessions = items;
        if self.selected_index.is_none() && !self.sessions.is_empty() {
            self.selected_index = Some(0);
        }
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.sessions.len() {
            return;
        }
        if self.selected_index == Some(index) {
            return;
        }
        self.selected_index = Some(index);
        let item = &self.sessions[index];
        cx.emit(SessionSelected {
            id: item.id.to_string(),
            label: item.label.clone(),
            agent: SharedString::from(item.agent.display_name().to_string()),
        });
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let now = Utc::now();

        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .w(px(256.0))
            .h_full()
            .bg(rgb(theme.bg_sidebar))
            .border_r_1()
            .border_color(rgb(theme.border))
            .font_family("IBM Plex Sans")
            .child(
                // Header
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_primary))
                            .child("Sessions"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{}", self.sessions.len())),
                    ),
            )
            .child({
                // Session list. Row rendering has to happen inside a closure
                // that owns `&mut Context<Sidebar>` so we can build per-row
                // `cx.listener` click handlers.
                let selected_index = self.selected_index;
                let rows: Vec<AnyElement> = self
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(idx, item)| {
                        self.render_session_row(
                            idx,
                            item,
                            selected_index == Some(idx),
                            now,
                            &theme,
                            cx,
                        )
                    })
                    .collect();
                div()
                    .id("session-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .py_1()
                    .children(rows)
            })
            .child(
                // Footer - servers section
                div()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_muted))
                            .child("SERVERS"),
                    )
                    .child({
                        let status = self.sidecar_status.lock().clone();
                        let (dot_color, label, sub) = describe_sidecar(&status, &theme);
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .py_1()
                            .child(
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(rgb(dot_color)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(theme.text_secondary))
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(theme.text_muted))
                                            .child(sub),
                                    ),
                            )
                    }),
            )
    }
}

impl Sidebar {
    fn render_session_row(
        &self,
        index: usize,
        item: &SessionItem,
        selected: bool,
        now: DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (dot_color, status_label) = status_visuals(&item.state, theme);
        let bg = if selected { theme.bg_active } else { theme.bg_sidebar };
        let hover_bg = if selected { theme.bg_active } else { theme.bg_hover };
        let age = relative_time(now, item.created_at);
        let subline = SharedString::from(format!(
            "{} • {} • {}",
            item.agent.display_name(),
            status_label,
            age
        ));

        div()
            .id(ElementId::Name(item.id.clone().into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mx_1()
            .px_2()
            .py_1p5()
            .rounded_md()
            .bg(rgb(bg))
            .hover(move |s| s.bg(rgb(hover_bg)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _evt, _window, cx| {
                this.select(index, cx);
            }))
            // Fixed-width leading icon column.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(12.0))
                    .justify_center()
                    .child(
                        div()
                            .size(px(8.0))
                            .rounded_full()
                            .bg(rgb(dot_color)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.text_primary))
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(item.label.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(subline),
                    ),
            )
            .into_any_element()
    }
}

/// Map a `SessionState` onto a (dot color, short status label).
fn status_visuals(state: &SessionState, theme: &Theme) -> (u32, SharedString) {
    match state {
        SessionState::Created => (theme.text_muted, "new".into()),
        SessionState::Booting => (theme.warning, "booting".into()),
        SessionState::Ready => (theme.success, "ready".into()),
        SessionState::Running => (theme.accent, "running".into()),
        SessionState::Paused => (theme.warning, "paused".into()),
        SessionState::Checkpointed => (theme.text_muted, "checkpointed".into()),
        SessionState::Forked => (theme.accent, "forked".into()),
        SessionState::Reviewing => (theme.warning, "review".into()),
        SessionState::Error(msg) => (
            theme.error,
            SharedString::from(format!("error: {}", truncate(msg, 24))),
        ),
        SessionState::Destroyed => (theme.text_muted, "destroyed".into()),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn relative_time(now: DateTime<Utc>, then: DateTime<Utc>) -> SharedString {
    let dur = now.signed_duration_since(then);
    let secs = dur.num_seconds().max(0);
    let out = if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    };
    SharedString::from(out)
}

fn describe_sidecar(
    status: &SidecarStatus,
    theme: &Theme,
) -> (u32, SharedString, SharedString) {
    match status {
        SidecarStatus::Idle => (
            theme.text_muted,
            "This Mac (local)".into(),
            "sidecar idle".into(),
        ),
        SidecarStatus::Installing => (
            theme.warning,
            "This Mac (local)".into(),
            "installing deps…".into(),
        ),
        SidecarStatus::Starting => (
            theme.warning,
            "This Mac (local)".into(),
            "starting rivet…".into(),
        ),
        SidecarStatus::Ready => (
            theme.success,
            "This Mac (local)".into(),
            "rivet ready on :6420".into(),
        ),
        SidecarStatus::Failed(msg) => (
            theme.error,
            "This Mac (local)".into(),
            SharedString::from(format!("sidecar failed: {msg}")),
        ),
    }
}
