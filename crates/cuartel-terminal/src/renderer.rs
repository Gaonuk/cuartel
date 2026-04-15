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
    headless: bool,
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
            headless: false,
            error: None,
            _poll_task: None,
        };
        view.start_shell(cx);
        view
    }

    /// Terminal view with no local PTY — all output is driven externally via
    /// [`TerminalView::write_bytes`] / [`TerminalView::write_text`]. Used by
    /// the Rivet session orchestrator to display remote agent output.
    pub fn new_headless(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            term: Terminal::new(DEFAULT_ROWS, DEFAULT_COLS),
            pty: None,
            focus_handle,
            focused_once: true,
            headless: true,
            error: None,
            _poll_task: None,
        }
    }

    /// Feed raw bytes into the grid parser, as if they had arrived from a
    /// PTY. Safe to call whether or not a local PTY is running.
    pub fn push_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        self.term.advance(bytes);
        cx.notify();
    }

    /// Convenience for feeding a UTF-8 string into the grid. Newlines are
    /// translated to CRLF so the parser positions the cursor correctly.
    pub fn push_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let mut buf = Vec::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            if let Some(stripped) = line.strip_suffix('\n') {
                buf.extend_from_slice(stripped.as_bytes());
                buf.extend_from_slice(b"\r\n");
            } else {
                buf.extend_from_slice(line.as_bytes());
            }
        }
        self.push_bytes(&buf, cx);
    }

    /// Display an error banner at the bottom of the view.
    pub fn set_error(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
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
        if !self.headless && !self.focused_once {
            window.focus(&self.focus_handle);
            self.focused_once = true;
        }

        let rows: Vec<Vec<Cell>> = self
            .term
            .grid
            .visible_rows()
            .map(|r| r.clone())
            .collect();
        let cursor_visible_row = self.term.grid.scrollback.len() + self.term.grid.cursor_row;
        let cursor_col = self.term.grid.cursor_col;
        let has_focus = self.focus_handle.is_focused(window);
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
            .children(rows.into_iter().enumerate().map(|(idx, row)| {
                let cursor = if idx == cursor_visible_row {
                    Some((cursor_col, has_focus))
                } else {
                    None
                };
                render_row(row, cursor)
            }))
    }
}

fn render_row(row: Vec<Cell>, cursor: Option<(usize, bool)>) -> Div {
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

    let cursor_col = cursor.map(|(c, _)| c);
    let cursor_focused = cursor.map(|(_, f)| f).unwrap_or(false);

    for (col, cell) in row.into_iter().enumerate() {
        if Some(col) == cursor_col {
            if let Some(s) = current_style.take() {
                flush(&mut children, s, &mut current_text);
            }
            children.push(render_cursor_cell(&cell, cursor_focused));
            continue;
        }
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

fn render_cursor_cell(cell: &Cell, focused: bool) -> AnyElement {
    let ch = if cell.ch == ' ' || cell.ch == '\0' {
        ' '
    } else {
        cell.ch
    };
    let text = ch.to_string();
    let cursor_bg = 0xcdd6f4u32; // light
    let cursor_fg = 0x11111bu32; // dark

    let mut el = div().child(text);
    if focused {
        el = el.bg(rgb(cursor_bg)).text_color(rgb(cursor_fg));
    } else {
        el = el
            .border_1()
            .border_color(rgb(cursor_bg))
            .text_color(fg_color(cell.style));
    }
    if cell.style.bold {
        el = el.font_weight(FontWeight::BOLD);
    }
    el.into_any_element()
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
