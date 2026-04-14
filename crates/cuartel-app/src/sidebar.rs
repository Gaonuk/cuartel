use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;
use gpui::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

pub struct Sidebar {
    sessions: Vec<SidebarItem>,
    selected_index: Option<usize>,
    sidecar_status: Arc<Mutex<SidecarStatus>>,
    last_seen_status: SidecarStatus,
    _poll_task: Task<()>,
}

#[derive(Clone)]
struct SidebarItem {
    id: String,
    label: SharedString,
    agent: SharedString,
    status: SessionStatus,
}

#[derive(Clone, PartialEq)]
enum SessionStatus {
    Running,
    Ready,
    Paused,
    Error,
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

    pub fn add_session(
        &mut self,
        id: String,
        label: &str,
        agent: &str,
        cx: &mut Context<Self>,
    ) {
        self.sessions.push(SidebarItem {
            id,
            label: SharedString::from(label.to_string()),
            agent: SharedString::from(agent.to_string()),
            status: SessionStatus::Ready,
        });
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .w(px(240.0))
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
                    ),
            )
            .child(
                // Session list
                div()
                    .id("session-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .px_1()
                    .children(self.sessions.iter().enumerate().map(|(idx, item)| {
                        let is_selected = self.selected_index == Some(idx);
                        let bg = if is_selected {
                            theme.bg_active
                        } else {
                            theme.bg_sidebar
                        };
                        let status_color = match item.status {
                            SessionStatus::Running => theme.accent,
                            SessionStatus::Ready => theme.success,
                            SessionStatus::Paused => theme.warning,
                            SessionStatus::Error => theme.error,
                        };

                        div()
                            .id(ElementId::Name(item.id.clone().into()))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .mx_1()
                            .rounded_md()
                            .bg(rgb(bg))
                            .child(
                                div()
                                    .size(px(8.0))
                                    .rounded_full()
                                    .bg(rgb(status_color)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(theme.text_primary))
                                            .child(item.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(theme.text_muted))
                                            .child(item.agent.clone()),
                                    ),
                            )
                    })),
            )
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
