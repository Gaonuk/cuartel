use crate::theme::Theme;
use cuartel_terminal::TerminalView;
use gpui::*;

pub struct WorkspaceView {
    terminal: Entity<TerminalView>,
    label: SharedString,
    agent: SharedString,
}

impl WorkspaceView {
    pub fn new(
        label: impl Into<SharedString>,
        agent: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let terminal = cx.new(|cx| TerminalView::new(cx));
        Self {
            terminal,
            label: label.into(),
            agent: agent.into(),
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
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        div()
            .id("workspace")
            .flex()
            .flex_col()
            .flex_1()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(
                // Tab bar
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
            .child(
                // Terminal area
                div()
                    .flex_1()
                    .child(self.terminal.clone()),
            )
    }
}
