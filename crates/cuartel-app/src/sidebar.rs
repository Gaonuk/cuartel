use crate::theme::Theme;
use gpui::*;

pub struct Sidebar {
    sessions: Vec<SidebarItem>,
    selected_index: Option<usize>,
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
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            sessions: vec![],
            selected_index: None,
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .py_1()
                            .child(
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(rgb(theme.success)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.text_secondary))
                                    .child("This Mac (local)"),
                            ),
                    ),
            )
    }
}
