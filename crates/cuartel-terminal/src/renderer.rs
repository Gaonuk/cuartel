use crate::grid::{Cell, CellStyle, Color, Row, Terminal};
use crate::pty::PtySession;
use gpui::*;
use std::cell::Cell as StdCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_ROWS: usize = 40;
const DEFAULT_COLS: usize = 120;
const LINE_HEIGHT: f32 = 18.0;
const FONT_SIZE: f32 = 13.0;
const FONT_FAMILY: &str = "Lilex";
// Conservative fallback for Lilex 13px advance width if metrics lookup fails.
const FALLBACK_CELL_WIDTH: f32 = 7.8;
const FG_DEFAULT: u32 = 0xcdd6f4;
const BG_DEFAULT: u32 = 0x11111b;
const SELECTION_BG: u32 = 0x45475a;
const SELECTION_FG: u32 = 0xcdd6f4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SelectionPoint {
    /// Row index into the combined scrollback + screen list.
    row: usize,
    col: usize,
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    anchor: SelectionPoint,
    head: SelectionPoint,
}

impl Selection {
    fn ordered(&self) -> (SelectionPoint, SelectionPoint) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// `[start, end)` columns to highlight on the given row, if any.
    fn cols_for_row(&self, row: usize, row_len: usize) -> Option<(usize, usize)> {
        let (start, end) = self.ordered();
        if row < start.row || row > end.row {
            return None;
        }
        let s = if row == start.row { start.col } else { 0 };
        let e = if row == end.row { end.col } else { row_len };
        if s >= e { return None; }
        Some((s, e))
    }
}

pub struct TerminalView {
    term: Terminal,
    pty: Option<Arc<PtySession>>,
    focus_handle: FocusHandle,
    focused_once: bool,
    headless: bool,
    error: Option<SharedString>,
    _poll_task: Option<Task<()>>,
    selection: Option<Selection>,
    selecting: bool,
    /// Origin (window coords) of the first row in the content area. Updated by
    /// a zero-height canvas on every paint so mouse math stays in sync with
    /// scroll / layout changes.
    content_origin: Rc<StdCell<Option<Point<Pixels>>>>,
    /// Advance width of a single monospace cell for the terminal font. Sampled
    /// from the text system on the first paint and reused after that.
    cell_width: Rc<StdCell<Option<Pixels>>>,
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
            selection: None,
            selecting: false,
            content_origin: Rc::new(StdCell::new(None)),
            cell_width: Rc::new(StdCell::new(None)),
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
            selection: None,
            selecting: false,
            content_origin: Rc::new(StdCell::new(None)),
            cell_width: Rc::new(StdCell::new(None)),
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

    fn visible_row_count(&self) -> usize {
        self.term.grid.scrollback.len() + self.term.grid.rows
    }

    fn point_from_position(&self, position: Point<Pixels>) -> Option<SelectionPoint> {
        let origin = self.content_origin.get()?;
        let cell_width = self
            .cell_width
            .get()
            .unwrap_or(px(FALLBACK_CELL_WIDTH));
        let dx = (f32::from(position.x) - f32::from(origin.x)).max(0.0);
        let dy = (f32::from(position.y) - f32::from(origin.y)).max(0.0);
        let row = (dy / LINE_HEIGHT) as usize;
        // Round to the nearest cell boundary so clicks near the right half of
        // a glyph snap forward (feels more natural when picking the end of a
        // word).
        let col = ((dx / f32::from(cell_width)) + 0.5) as usize;
        let rows = self.visible_row_count();
        let row = row.min(rows.saturating_sub(1));
        let col = col.min(self.term.grid.cols);
        Some(SelectionPoint { row, col })
    }

    fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let (start, end) = sel.ordered();
        if start == end {
            return None;
        }
        let rows: Vec<&Row> = self.term.grid.visible_rows().collect();
        let mut out = String::new();
        for row_idx in start.row..=end.row {
            let Some(row) = rows.get(row_idx) else { break };
            let row_len = row.len();
            let (s, e) = if start.row == end.row {
                (start.col.min(row_len), end.col.min(row_len))
            } else if row_idx == start.row {
                (start.col.min(row_len), row_len)
            } else if row_idx == end.row {
                (0, end.col.min(row_len))
            } else {
                (0, row_len)
            };
            let mut line = String::with_capacity(e.saturating_sub(s));
            for cell in &row[s..e] {
                let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                line.push(ch);
            }
            // Trailing spaces within a line are almost always padding from the
            // grid model rather than meaningful whitespace — trim them so
            // pasted output doesn't carry huge runs of spaces.
            let trimmed_len = line.trim_end_matches(' ').len();
            line.truncate(trimmed_len);
            out.push_str(&line);
            if row_idx < end.row {
                out.push('\n');
            }
        }
        if out.is_empty() { None } else { Some(out) }
    }

    fn copy_selection(&self, cx: &mut App) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        // Ensure we receive the subsequent Cmd+C / keyboard events even in
        // headless mode, where the terminal is never auto-focused.
        window.focus(&self.focus_handle);
        if let Some(pt) = self.point_from_position(event.position) {
            self.selection = Some(Selection { anchor: pt, head: pt });
            self.selecting = true;
            cx.notify();
        }
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The mouse up happened outside our bounds — end the drag.
            self.selecting = false;
            if let Some(sel) = self.selection {
                if sel.anchor == sel.head {
                    self.selection = None;
                }
            }
            cx.notify();
            return;
        }
        if let Some(pt) = self.point_from_position(event.position) {
            if let Some(sel) = self.selection.as_mut() {
                sel.head = pt;
                cx.notify();
            }
        }
    }

    fn handle_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        if let Some(sel) = self.selection {
            if sel.anchor == sel.head {
                self.selection = None;
            }
        }
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let mods = &ks.modifiers;
        let key = ks.key.as_str();

        // Cmd+C (or Cmd+Shift+C) copies the current selection regardless of
        // headless state. Ctrl+C still falls through to the PTY as SIGINT.
        if mods.platform && !mods.control && !mods.alt && key == "c" {
            if self.selection.is_some() {
                self.copy_selection(cx);
                return;
            }
        }

        let Some(pty) = self.pty.clone() else { return };

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
        let selection = self.selection;

        // Canvas element that samples the content origin and terminal cell
        // width on every paint. We keep it zero-height so it doesn't affect
        // layout — its sole purpose is to thread `Bounds` / `Window` access
        // into the mouse-coordinate math.
        let origin_slot = self.content_origin.clone();
        let width_slot = self.cell_width.clone();
        let metrics_canvas = canvas(
            move |bounds, window, _cx| {
                origin_slot.set(Some(bounds.origin));
                if width_slot.get().is_none() {
                    let font = Font {
                        family: FONT_FAMILY.into(),
                        features: FontFeatures::default(),
                        fallbacks: None,
                        weight: FontWeight::default(),
                        style: FontStyle::default(),
                    };
                    let font_id = window.text_system().resolve_font(&font);
                    if let Ok(width) =
                        window.text_system().em_advance(font_id, px(FONT_SIZE))
                    {
                        width_slot.set(Some(width));
                    }
                }
            },
            |_, _, _, _| {},
        )
        .h(px(0.0));

        div()
            .id("terminal")
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::handle_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_mouse_down))
            .on_mouse_move(cx.listener(Self::handle_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::handle_mouse_up))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG_DEFAULT))
            .p_2()
            .overflow_y_scroll()
            .font_family(FONT_FAMILY)
            .text_size(px(FONT_SIZE))
            .text_color(rgb(FG_DEFAULT))
            .cursor_text()
            .children(error.map(|e| {
                div().text_color(rgb(0xf38ba8)).child(e)
            }))
            .child(metrics_canvas)
            .children(rows.into_iter().enumerate().map(|(idx, row)| {
                let cursor = if idx == cursor_visible_row {
                    Some((cursor_col, has_focus))
                } else {
                    None
                };
                let row_selection = selection
                    .and_then(|s| s.cols_for_row(idx, row.len()));
                render_row(row, cursor, row_selection)
            }))
    }
}

fn render_row(
    row: Vec<Cell>,
    cursor: Option<(usize, bool)>,
    selection_cols: Option<(usize, usize)>,
) -> Div {
    // Group runs of same-style cells so we emit one child per run.
    let mut children: Vec<AnyElement> = Vec::new();
    let mut current_style: Option<CellStyle> = None;
    let mut current_text = String::new();
    let mut current_selected = false;

    let flush = |children: &mut Vec<AnyElement>,
                 style: CellStyle,
                 text: &mut String,
                 selected: bool| {
        if text.is_empty() {
            return;
        }
        let mut el = div().child(std::mem::take(text));
        if selected {
            el = el.bg(rgb(SELECTION_BG)).text_color(rgb(SELECTION_FG));
        } else {
            el = el.text_color(fg_color(style));
            match style.bg {
                Color::Default => {}
                Color::Indexed(idx) => {
                    el = el.bg(rgb(palette_color(idx)));
                }
                Color::Rgb(r, g, b) => {
                    el = el.bg(rgb(rgb_u32(r, g, b)));
                }
            }
        }
        if style.bold {
            el = el.font_weight(FontWeight::BOLD);
        }
        children.push(el.into_any_element());
    };

    let cursor_col = cursor.map(|(c, _)| c);
    let cursor_focused = cursor.map(|(_, f)| f).unwrap_or(false);

    for (col, cell) in row.into_iter().enumerate() {
        if Some(col) == cursor_col {
            if let Some(s) = current_style.take() {
                flush(&mut children, s, &mut current_text, current_selected);
            }
            children.push(render_cursor_cell(&cell, cursor_focused));
            continue;
        }
        let selected = selection_cols
            .map(|(s, e)| col >= s && col < e)
            .unwrap_or(false);
        match current_style {
            Some(s) if s == cell.style && selected == current_selected => {
                current_text.push(cell.ch);
            }
            _ => {
                if let Some(s) = current_style {
                    flush(&mut children, s, &mut current_text, current_selected);
                }
                current_style = Some(cell.style);
                current_selected = selected;
                current_text.push(cell.ch);
            }
        }
    }
    if let Some(s) = current_style {
        flush(&mut children, s, &mut current_text, current_selected);
    }

    div()
        .flex()
        .flex_row()
        .h(px(LINE_HEIGHT))
        .children(children)
}

fn render_cursor_cell(cell: &Cell, focused: bool) -> AnyElement {
    let ch = if cell.ch == ' ' || cell.ch == '\0' {
        ' '
    } else {
        cell.ch
    };
    let text = ch.to_string();
    let cursor_bg = FG_DEFAULT;
    let cursor_fg = BG_DEFAULT;

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
        Color::Default => FG_DEFAULT,
        Color::Indexed(idx) => palette_color(idx),
        Color::Rgb(r, g, b) => rgb_u32(r, g, b),
    };
    rgb(base)
}

fn rgb_u32(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// 256-color xterm palette. 0-15 follow the official Catppuccin Mocha terminal
/// spec so output from agents running in a Mocha-themed VM matches what you'd
/// see in a local terminal; 16-231 are the standard 6x6x6 cube; 232-255 the
/// grayscale ramp.
fn palette_color(idx: u8) -> u32 {
    // https://github.com/catppuccin/catppuccin#-palette (Mocha)
    const BASIC: [u32; 16] = [
        0x45475a, // 0  black           (surface1)
        0xf38ba8, // 1  red
        0xa6e3a1, // 2  green
        0xf9e2af, // 3  yellow
        0x89b4fa, // 4  blue
        0xf5c2e7, // 5  magenta (pink)
        0x94e2d5, // 6  cyan (teal)
        0xbac2de, // 7  white  (subtext1)
        0x585b70, // 8  bright black    (surface2)
        0xf38ba8, // 9  bright red
        0xa6e3a1, // 10 bright green
        0xf9e2af, // 11 bright yellow
        0x89b4fa, // 12 bright blue
        0xf5c2e7, // 13 bright magenta
        0x94e2d5, // 14 bright cyan
        0xa6adc8, // 15 bright white    (subtext0)
    ];
    match idx {
        0..=15 => BASIC[idx as usize],
        16..=231 => {
            // Standard xterm 6x6x6 cube levels: 0, 95, 135, 175, 215, 255.
            const LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
            let i = idx - 16;
            let r = LEVELS[(i / 36) as usize];
            let g = LEVELS[((i % 36) / 6) as usize];
            let b = LEVELS[(i % 6) as usize];
            (r << 16) | (g << 8) | b
        }
        _ => {
            // Standard xterm grayscale ramp: 8, 18, 28, ..., 238.
            let v = 8 + (idx - 232) as u32 * 10;
            (v << 16) | (v << 8) | v
        }
    }
}
