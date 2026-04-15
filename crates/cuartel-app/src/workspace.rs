use crate::permission_prompt::PermissionPrompt;
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

pub struct WorkspaceView {
    terminal: Entity<TerminalView>,
    permission_prompt: Entity<PermissionPrompt>,
    label: SharedString,
    agent: SharedString,
    prompt_text: String,
    session_state: SessionState,
    focus_handle: FocusHandle,
    _observer: Subscription,
}

impl WorkspaceView {
    pub fn new(
        label: impl Into<SharedString>,
        agent: impl Into<SharedString>,
        terminal: Entity<TerminalView>,
        permission_prompt: Entity<PermissionPrompt>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observer = cx.observe(&permission_prompt, |_, _, cx| cx.notify());
        let focus_handle = cx.focus_handle();
        Self {
            terminal,
            permission_prompt,
            label: label.into(),
            agent: agent.into(),
            prompt_text: String::new(),
            session_state: SessionState::Created,
            focus_handle,
            _observer: observer,
        }
    }

    pub fn set_active_session(
        &mut self,
        label: SharedString,
        agent: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.label = label;
        self.agent = agent;
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

        div()
            .id("workspace")
            .flex()
            .flex_col()
            .flex_1()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(36.0))
                    .bg(rgb(theme.bg_secondary))
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .px_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(theme.bg_primary))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.text_primary))
                                    .child(self.label.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme.text_muted))
                                    .child(self.agent.clone()),
                            ),
                    ),
            )
            .children(show_prompt.then(|| self.permission_prompt.clone()))
            .child(div().flex_1().child(self.terminal.clone()))
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
