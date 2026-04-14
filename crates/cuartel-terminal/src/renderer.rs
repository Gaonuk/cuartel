use crate::grid::{Cell, CellStyle, Color, Terminal};
use crate::pty::PtySession;
use gpui::*;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_ROWS: usize = 40;
const DEFAULT_COLS: usize = 120;

pub struct TerminalView {
    term: Terminal,
    pty: Option<Arc<PtySession>>,
    focus_handle: FocusHandle,
    focused_once: bool,
    error: Option<SharedString>,
    _poll_task: Option<Task<()>>,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let mut view = Self {
            term: Terminal::new(DEFAULT_ROWS, DEFAULT_COLS),
            pty: None,
            focus_handle,
            focused_once: false,
            error: None,
            _poll_task: None,
        };
        view.start_shell(cx);
        view
    }

    fn start_shell(&mut self, cx: &mut Context<Self>) {
        match PtySession::spawn_shell(DEFAULT_ROWS as u16, DEFAULT_COLS as u16) {
            Ok(session) => {
                let session = Arc::new(session);
                self.pty = Some(session.clone());
                self._poll_task = Some(cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                        let chunk = session.drain_output();
                        if this
                            .update(cx, |view, cx| {
                                if !chunk.is_empty() {
                                    view.term.advance(&chunk);
                                    cx.notify();
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(e) => {
                log::error!("failed to spawn pty: {e}");
                self.error = Some(format!("failed to spawn shell: {e}").into());
            }
        }
    }

    pub fn write_bytes(&self, bytes: &[u8]) {
        if let Some(pty) = &self.pty {
            pty.write(bytes);
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(pty) = self.pty.clone() else { return };
        let ks = &event.keystroke;
        let mods = &ks.modifiers;
        let key = ks.key.as_str();

        // Ctrl + letter → 0x01..0x1a
        if mods.control && key.len() == 1 {
            let b = key.as_bytes()[0];
            if b.is_ascii_alphabetic() {
                let c = (b.to_ascii_lowercase() - b'a' + 1) as u8;
                pty.write(&[c]);
                cx.notify();
                return;
            }
            match key {
                "[" => { pty.write(&[0x1b]); cx.notify(); return; }
                "\\" => { pty.write(&[0x1c]); cx.notify(); return; }
                "]" => { pty.write(&[0x1d]); cx.notify(); return; }
                " " => { pty.write(&[0x00]); cx.notify(); return; }
                _ => {}
            }
        }

        let bytes: &[u8] = match key {
            "enter" => b"\r",
            "tab" => b"\t",
            "backspace" => b"\x7f",
            "escape" => b"\x1b",
            "delete" => b"\x1b[3~",
            "up" => b"\x1b[A",
            "down" => b"\x1b[B",
            "right" => b"\x1b[C",
            "left" => b"\x1b[D",
            "home" => b"\x1b[H",
            "end" => b"\x1b[F",
            "pageup" => b"\x1b[5~",
            "pagedown" => b"\x1b[6~",
            _ => {
                if let Some(text) = ks.key_char.as_ref() {
                    pty.write(text.as_bytes());
                    cx.notify();
                }
                return;
            }
        };
        pty.write(bytes);
        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_once {
            window.focus(&self.focus_handle);
            self.focused_once = true;
        }

        let rows: Vec<Vec<Cell>> = self
            .term
            .grid
            .visible_rows()
            .map(|r| r.clone())
            .collect();
        let error = self.error.clone();

        div()
            .id("terminal")
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::handle_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x11111b))
            .p_2()
            .overflow_y_scroll()
            .font_family("Lilex")
            .text_size(px(13.0))
            .text_color(rgb(0xcdd6f4))
            .children(error.map(|e| {
                div().text_color(rgb(0xf38ba8)).child(e)
            }))
            .children(rows.into_iter().map(render_row))
    }
}

fn render_row(row: Vec<Cell>) -> Div {
    // Group runs of same-style cells so we emit one child per run.
    let mut children: Vec<AnyElement> = Vec::new();
    let mut current_style: Option<CellStyle> = None;
    let mut current_text = String::new();

    let flush = |children: &mut Vec<AnyElement>, style: CellStyle, text: &mut String| {
        if text.is_empty() { return; }
        let fg = fg_color(style);
        let mut el = div().child(std::mem::take(text));
        el = el.text_color(fg);
        if style.bold {
            el = el.font_weight(FontWeight::BOLD);
        }
        if let Color::Indexed(idx) = style.bg {
            el = el.bg(rgb(palette_color(idx)));
        }
        children.push(el.into_any_element());
    };

    for cell in row {
        match current_style {
            Some(s) if s == cell.style => {
                current_text.push(cell.ch);
            }
            _ => {
                if let Some(s) = current_style {
                    flush(&mut children, s, &mut current_text);
                }
                current_style = Some(cell.style);
                current_text.push(cell.ch);
            }
        }
    }
    if let Some(s) = current_style {
        flush(&mut children, s, &mut current_text);
    }

    div()
        .flex()
        .flex_row()
        .h(px(18.0))
        .children(children)
}

fn fg_color(style: CellStyle) -> Rgba {
    let base = match style.fg {
        Color::Default => 0xcdd6f4,
        Color::Indexed(idx) => palette_color(idx),
    };
    rgb(base)
}

/// 256-color xterm palette. 0-15 are the basic colors (Catppuccin-ish),
/// 16-231 the 6x6x6 cube, 232-255 the grayscale ramp.
fn palette_color(idx: u8) -> u32 {
    const BASIC: [u32; 16] = [
        0x45475a, // 0 black
        0xf38ba8, // 1 red
        0xa6e3a1, // 2 green
        0xf9e2af, // 3 yellow
        0x89b4fa, // 4 blue
        0xcba6f7, // 5 magenta
        0x94e2d5, // 6 cyan
        0xbac2de, // 7 white
        0x585b70, // 8 bright black
        0xf37799, // 9 bright red
        0x94d98d, // 10 bright green
        0xf5cf78, // 11 bright yellow
        0x74a7f5, // 12 bright blue
        0xc094f5, // 13 bright magenta
        0x7ed7c9, // 14 bright cyan
        0xcdd6f4, // 15 bright white
    ];
    match idx {
        0..=15 => BASIC[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let scale = |c: u8| -> u32 { if c == 0 { 0 } else { (55 + c as u32 * 40) & 0xff } };
            (scale(r) << 16) | (scale(g) << 8) | scale(b)
        }
        _ => {
            let v = 8 + (idx - 232) as u32 * 10;
            (v << 16) | (v << 8) | v
        }
    }
}
