use crate::diff_view::{DiffView, ReviewApply};
use crate::permission_prompt::PermissionPrompt;
use crate::tab_bar::TabBar;
use crate::theme::Theme;
use cuartel_core::session::SessionState;
use cuartel_terminal::TerminalView;
use gpui::prelude::FluentBuilder;
use gpui::*;

#[derive(Clone, Debug)]
pub struct PromptSubmitted {
    pub text: String,
}

impl EventEmitter<PromptSubmitted> for WorkspaceView {}
impl EventEmitter<ReviewApply> for WorkspaceView {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceTab {
    Terminal,
    Review,
}

pub struct WorkspaceView {
    tab_bar: Entity<TabBar>,
    terminal: Entity<TerminalView>,
    diff_view: Entity<DiffView>,
    permission_prompt: Entity<PermissionPrompt>,
    prompt_text: String,
    session_state: SessionState,
    active_tab: WorkspaceTab,
    focus_handle: FocusHandle,
    _observer: Subscription,
    _review_sub: Subscription,
}

impl WorkspaceView {
    pub fn new(
        tab_bar: Entity<TabBar>,
        terminal: Entity<TerminalView>,
        diff_view: Entity<DiffView>,
        permission_prompt: Entity<PermissionPrompt>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observer = cx.observe(&permission_prompt, |_, _, cx| cx.notify());
        let review_sub =
            cx.subscribe(&diff_view, |this: &mut Self, _dv, event: &ReviewApply, cx| {
                cx.emit(event.clone());
                let _ = this;
            });
        let focus_handle = cx.focus_handle();
        Self {
            tab_bar,
            terminal,
            diff_view,
            permission_prompt,
            prompt_text: String::new(),
            session_state: SessionState::Created,
            active_tab: WorkspaceTab::Terminal,
            focus_handle,
            _observer: observer,
            _review_sub: review_sub,
        }
    }

    pub fn swap_views(
        &mut self,
        terminal: Entity<TerminalView>,
        diff_view: Entity<DiffView>,
        permission_prompt: Entity<PermissionPrompt>,
        cx: &mut Context<Self>,
    ) {
        self.terminal = terminal;
        self.diff_view = diff_view.clone();
        self.permission_prompt = permission_prompt.clone();
        self._observer = cx.observe(&permission_prompt, |_, _, cx| cx.notify());
        self._review_sub =
            cx.subscribe(&diff_view, |this: &mut Self, _dv, event: &ReviewApply, cx| {
                cx.emit(event.clone());
                let _ = this;
            });
        cx.notify();
    }

    fn set_tab(&mut self, tab: WorkspaceTab, cx: &mut Context<Self>) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        cx.notify();
    }

    pub fn set_session_state(&mut self, state: SessionState, cx: &mut Context<Self>) {
        self.session_state = state;
        cx.notify();
    }

    fn can_send_prompt(&self) -> bool {
        matches!(self.session_state, SessionState::Ready)
    }

    fn submit_prompt(&mut self, cx: &mut Context<Self>) {
        if !self.can_send_prompt() {
            return;
        }
        let text = self.prompt_text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.prompt_text.clear();
        cx.emit(PromptSubmitted { text });
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let mods = &ks.modifiers;

        if mods.control || mods.alt || mods.platform {
            return;
        }

        match ks.key.as_str() {
            "enter" => {
                self.submit_prompt(cx);
            }
            "backspace" => {
                self.prompt_text.pop();
                cx.notify();
            }
            "escape" => {
                self.prompt_text.clear();
                cx.notify();
            }
            _ => {
                if let Some(ch) = ks.key_char.as_ref() {
                    self.prompt_text.push_str(ch.as_str());
                    cx.notify();
                }
            }
        }
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let show_prompt = !self.permission_prompt.read(cx).is_empty();
        let can_send = self.can_send_prompt();
        let is_booting = matches!(
            self.session_state,
            SessionState::Created | SessionState::Booting
        );
        let prompt_placeholder = if is_booting {
            "Booting session..."
        } else if matches!(self.session_state, SessionState::Running) {
            "Agent is running..."
        } else if matches!(self.session_state, SessionState::Destroyed) {
            "Session ended"
        } else if can_send {
            "Type a prompt and press Enter..."
        } else {
            "Session not ready"
        };

        window.focus(&self.focus_handle);

        let active_tab = self.active_tab;
        let review_count = self.diff_view.read(cx).len();
        let review_label = if review_count > 0 {
            SharedString::from(format!("Review ({review_count})"))
        } else {
            SharedString::from("Review")
        };

        let tab_button = |id: &'static str,
                          label: SharedString,
                          tab: WorkspaceTab,
                          theme: &Theme,
                          cx: &mut Context<Self>| {
            let active = active_tab == tab;
            let bg = if active { theme.bg_primary } else { theme.bg_secondary };
            let fg = if active { theme.text_primary } else { theme.text_muted };
            let border = if active { theme.accent } else { theme.bg_secondary };
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .bg(rgb(bg))
                .border_b_2()
                .border_color(rgb(border))
                .text_sm()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(rgb(fg))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(theme.bg_hover)))
                .on_click(cx.listener(move |this, _evt, _win, cx| this.set_tab(tab, cx)))
                .child(label)
        };

        let body: AnyElement = match self.active_tab {
            WorkspaceTab::Terminal => div()
                .flex_1()
                .child(self.terminal.clone())
                .into_any_element(),
            WorkspaceTab::Review => div()
                .flex_1()
                .min_h_0()
                .child(self.diff_view.clone())
                .into_any_element(),
        };

        div()
            .id("workspace")
            .flex()
            .flex_col()
            .flex_1()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            // Session tab bar
            .child(self.tab_bar.clone())
            // Terminal / Review mode tabs
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.0))
                    .bg(rgb(theme.bg_secondary))
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .px_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(tab_button(
                                "tab-terminal",
                                "Terminal".into(),
                                WorkspaceTab::Terminal,
                                &theme,
                                cx,
                            ))
                            .child(tab_button(
                                "tab-review",
                                review_label,
                                WorkspaceTab::Review,
                                &theme,
                                cx,
                            )),
                    ),
            )
            .children(show_prompt.then(|| self.permission_prompt.clone()))
            .child(body)
            .child(
                div()
                    .id("prompt-bar")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::handle_key_down))
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(36.0))
                    .bg(rgb(theme.bg_secondary))
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .px_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(if can_send {
                                theme.accent
                            } else {
                                theme.text_muted
                            }))
                            .child(">"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .px_2()
                            .text_sm()
                            .text_color(rgb(if self.prompt_text.is_empty() {
                                theme.text_muted
                            } else {
                                theme.text_primary
                            }))
                            .child(if self.prompt_text.is_empty() {
                                SharedString::from(prompt_placeholder)
                            } else {
                                SharedString::from(self.prompt_text.clone())
                            }),
                    )
                    .when(self.focus_handle.is_focused(window), |el| {
                        el.child(div().w(px(8.0)).h(px(14.0)).bg(rgb(theme.text_primary)))
                    }),
            )
    }
}
