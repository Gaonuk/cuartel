use crate::theme::Theme;
use cuartel_terminal::TerminalView;
use gpui::*;

pub struct WorkspaceView {
    terminal: Entity<TerminalView>,
    name: SharedString,
}

impl WorkspaceView {
    pub fn new(name: &str, cx: &mut Context<Self>) -> Self {
        let terminal = cx.new(|cx| TerminalView::new(cx));
        Self {
            terminal,
            name: SharedString::from(name.to_string()),
        }
    }

    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
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
                            .gap_1()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(theme.bg_primary))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.text_primary))
                                    .child(self.name.clone()),
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
