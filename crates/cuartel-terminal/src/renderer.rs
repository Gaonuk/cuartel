use crate::terminal::TerminalBuffer;
use gpui::*;

pub struct TerminalView {
    buffer: TerminalBuffer,
}

impl TerminalView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            buffer: TerminalBuffer::new(10_000),
        }
    }

    pub fn push_output(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.push_text(text);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer.clear();
        cx.notify();
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.buffer.lines().to_vec();

        div()
            .id("terminal")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e2e))
            .p_2()
            .overflow_y_scroll()
            .font_family("Lilex")
            .text_sm()
            .text_color(rgb(0xcdd6f4))
            .children(lines.into_iter().map(|line| {
                div().child(line)
            }))
    }
}
