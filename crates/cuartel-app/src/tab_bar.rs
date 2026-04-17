use crate::theme::Theme;
use cuartel_core::agent::AgentType;
use cuartel_core::session::SessionState;
use gpui::prelude::FluentBuilder;
use gpui::*;

#[derive(Clone, Debug)]
pub struct TabSelected {
    pub session_id: String,
}

#[derive(Clone, Debug)]
pub struct NewTabRequested;

#[derive(Clone, Debug)]
pub struct TabCloseRequested {
    pub session_id: String,
}

impl EventEmitter<TabSelected> for TabBar {}
impl EventEmitter<NewTabRequested> for TabBar {}
impl EventEmitter<TabCloseRequested> for TabBar {}

#[derive(Clone, Debug)]
pub struct TabInfo {
    pub session_id: String,
    pub label: SharedString,
    pub agent: AgentType,
    pub state: SessionState,
}

pub struct TabBar {
    tabs: Vec<TabInfo>,
    active_id: Option<String>,
}

impl TabBar {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            tabs: Vec::new(),
            active_id: None,
        }
    }

    pub fn set_tabs(&mut self, tabs: Vec<TabInfo>, cx: &mut Context<Self>) {
        self.tabs = tabs;
        cx.notify();
    }

    pub fn set_active(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.active_id = Some(session_id.to_string());
        cx.notify();
    }

    pub fn update_tab_state(
        &mut self,
        session_id: &str,
        state: SessionState,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.session_id == session_id) {
            tab.state = state;
            cx.notify();
        }
    }
}

impl Render for TabBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let can_close = self.tabs.len() > 1;

        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(_idx, tab)| {
                let active = self.active_id.as_deref() == Some(&tab.session_id);
                let sid = tab.session_id.clone();
                let sid_close = tab.session_id.clone();
                let (dot_color, _status) = state_dot(&tab.state, &theme);

                let bg = if active {
                    theme.bg_primary
                } else {
                    theme.bg_secondary
                };
                let fg = if active {
                    theme.text_primary
                } else {
                    theme.text_muted
                };
                let border = if active {
                    theme.accent
                } else {
                    theme.bg_secondary
                };

                div()
                    .id(ElementId::Name(
                        SharedString::from(format!("tab-{}", tab.session_id)).into(),
                    ))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .px_3()
                    .py_1()
                    .bg(rgb(bg))
                    .border_b_2()
                    .border_color(rgb(border))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .on_click(cx.listener(move |this, _evt, _win, cx| {
                        this.active_id = Some(sid.clone());
                        cx.emit(TabSelected {
                            session_id: sid.clone(),
                        });
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(rgb(dot_color)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(if active {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(rgb(fg))
                            .child(tab.label.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(SharedString::from(tab.agent.display_name().to_string())),
                    )
                    .when(can_close, |el| {
                        el.child(
                            div()
                                .id(ElementId::Name(
                                    SharedString::from(format!("close-{}", sid_close)).into(),
                                ))
                                .text_xs()
                                .text_color(rgb(theme.text_muted))
                                .cursor_pointer()
                                .hover(|s| s.text_color(rgb(theme.error)))
                                .on_click(cx.listener(move |_this, _evt, _win, cx| {
                                    cx.emit(TabCloseRequested {
                                        session_id: sid_close.clone(),
                                    });
                                }))
                                .child("x"),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .items_center()
            .h(px(32.0))
            .bg(rgb(theme.bg_secondary))
            .border_b_1()
            .border_color(rgb(theme.border))
            .children(tabs)
            .child(
                div()
                    .id("new-tab-btn")
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(rgb(theme.text_muted))
                    .cursor_pointer()
                    .hover(|s| s.text_color(rgb(theme.accent)))
                    .on_click(cx.listener(|_this, _evt, _win, cx| {
                        cx.emit(NewTabRequested);
                    }))
                    .child("+"),
            )
    }
}

fn state_dot(state: &SessionState, theme: &Theme) -> (u32, &'static str) {
    match state {
        SessionState::Created => (theme.text_muted, "new"),
        SessionState::Booting => (theme.warning, "booting"),
        SessionState::Ready => (theme.success, "ready"),
        SessionState::Running => (theme.accent, "running"),
        SessionState::Paused => (theme.warning, "paused"),
        SessionState::Checkpointed => (theme.text_muted, "checkpoint"),
        SessionState::Forked => (theme.accent, "forked"),
        SessionState::Reviewing => (theme.warning, "review"),
        SessionState::Error(_) => (theme.error, "error"),
        SessionState::Destroyed => (theme.text_muted, "ended"),
    }
}
