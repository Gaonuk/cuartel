use crate::session_host::AgentMode;
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

#[derive(Clone, Debug)]
pub struct AgentModeSelected {
    pub mode: AgentMode,
}

#[derive(Clone, Debug)]
pub struct AgentTypeSelected {
    pub agent: AgentType,
}

impl EventEmitter<TabSelected> for TabBar {}
impl EventEmitter<NewTabRequested> for TabBar {}
impl EventEmitter<TabCloseRequested> for TabBar {}
impl EventEmitter<AgentModeSelected> for TabBar {}
impl EventEmitter<AgentTypeSelected> for TabBar {}

#[derive(Clone, Debug)]
pub struct TabInfo {
    pub session_id: String,
    pub label: SharedString,
    pub agent: AgentType,
    pub agent_mode: AgentMode,
    pub state: SessionState,
}

pub struct TabBar {
    tabs: Vec<TabInfo>,
    active_id: Option<String>,
    next_agent_mode: AgentMode,
    /// Which CLI flavor the next Native-mode session will spawn. Only
    /// surfaced as a sub-picker when `next_agent_mode == NativeClaudeCli`;
    /// other modes route through the harness layer and ignore this.
    next_agent_type: AgentType,
}

impl TabBar {
    pub fn new(
        initial_mode: AgentMode,
        initial_agent: AgentType,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            active_id: None,
            next_agent_mode: initial_mode,
            next_agent_type: initial_agent,
        }
    }

    /// Called from `CuartelApp` when the default agent changes (via
    /// onboarding / settings) so the picker shows the new default.
    pub fn set_next_agent_type(&mut self, agent: AgentType, cx: &mut Context<Self>) {
        self.next_agent_type = agent;
        cx.notify();
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
                    .child(
                        div()
                            .text_xs()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(theme.bg_primary))
                            .text_color(rgb(theme.accent))
                            .child(SharedString::from(tab.agent_mode.short_label())),
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

        let mode_picker = render_mode_picker(self.next_agent_mode, &theme, cx);
        let cli_picker = if self.next_agent_mode == AgentMode::NativeClaudeCli {
            Some(render_cli_picker(self.next_agent_type.clone(), &theme, cx))
        } else {
            None
        };

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
            .child(div().flex_1())
            .children(cli_picker)
            .child(mode_picker)
    }
}

/// Right-aligned 3-segment button group. The selected mode applies to
/// the next session created via the "+" tab button. Click any segment
/// to rebind; the change is local until session creation.
fn render_mode_picker(
    current: AgentMode,
    theme: &Theme,
    cx: &mut Context<TabBar>,
) -> impl IntoElement {
    let segments: Vec<AnyElement> = AgentMode::ALL
        .iter()
        .map(|mode| {
            let mode = *mode;
            let active = mode == current;
            let bg = if active { theme.bg_primary } else { theme.bg_secondary };
            let fg = if active { theme.text_primary } else { theme.text_muted };
            let border = if active { theme.accent } else { theme.bg_secondary };
            div()
                .id(ElementId::Name(
                    SharedString::from(format!("mode-{}", mode.short_label())).into(),
                ))
                .px_2()
                .py_0p5()
                .text_xs()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(rgb(fg))
                .bg(rgb(bg))
                .border_b_2()
                .border_color(rgb(border))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(theme.accent)))
                .on_click(cx.listener(move |this, _evt, _win, cx| {
                    this.next_agent_mode = mode;
                    cx.emit(AgentModeSelected { mode });
                    cx.notify();
                }))
                .child(SharedString::from(mode.short_label()))
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_row()
        .items_center()
        .pr_2()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.text_muted))
                .pr_1()
                .child("mode:"),
        )
        .children(segments)
}

/// CLI flavor sub-picker shown only when the mode picker is on Native.
/// Lists every entry of [`AgentType::all_native_cli`] as a clickable
/// segment; clicking emits `AgentTypeSelected` so `CuartelApp` can
/// rebind `next_agent_type` for the next "+" tab.
fn render_cli_picker(
    current: AgentType,
    theme: &Theme,
    cx: &mut Context<TabBar>,
) -> impl IntoElement {
    let segments: Vec<AnyElement> = AgentType::all_native_cli()
        .into_iter()
        .map(|agent| {
            let active = agent == current;
            let bg = if active { theme.bg_primary } else { theme.bg_secondary };
            let fg = if active { theme.text_primary } else { theme.text_muted };
            let border = if active { theme.accent } else { theme.bg_secondary };
            let id = format!("cli-{}", agent.rivet_name());
            let agent_for_click = agent.clone();
            div()
                .id(ElementId::Name(SharedString::from(id).into()))
                .px_2()
                .py_0p5()
                .text_xs()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(rgb(fg))
                .bg(rgb(bg))
                .border_b_2()
                .border_color(rgb(border))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(theme.accent)))
                .on_click(cx.listener(move |this, _evt, _win, cx| {
                    this.next_agent_type = agent_for_click.clone();
                    cx.emit(AgentTypeSelected {
                        agent: agent_for_click.clone(),
                    });
                    cx.notify();
                }))
                .child(SharedString::from(agent.short_label().to_string()))
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_row()
        .items_center()
        .pr_3()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.text_muted))
                .pr_1()
                .child("cli:"),
        )
        .children(segments)
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
