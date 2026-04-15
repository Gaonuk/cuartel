//! Minimal VT grid model: good enough for interactive shells, not vim/tmux.
//!
//! A `vte::Parser` feeds a `GridState` that maintains a fixed-size screen plus
//! an unbounded scrollback. We implement enough of the VT protocol to handle:
//!
//! - printable chars / UTF-8
//! - CR, LF, BS, BEL
//! - CSI A/B/C/D (cursor movement)
//! - CSI H / f    (cursor position)
//! - CSI J        (erase display, parts 0/1/2)
//! - CSI K        (erase in line, parts 0/1/2)
//! - CSI P        (delete chars)
//! - CSI m        (SGR: reset, bold, fg/bg 16-color + 256)
//!
//! Full-screen TUIs like vim/htop will render poorly — that's fine for now.

use vte::{Params, Parser, Perform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self { fg: Color::Default, bg: Color::Default, bold: false }
    }
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub ch: char,
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', style: CellStyle::default() }
    }
}

pub type Row = Vec<Cell>;

pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub screen: Vec<Row>,     // rows x cols
    pub scrollback: Vec<Row>, // oldest first
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub style: CellStyle,
    scrollback_limit: usize,
}

impl Grid {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            screen: (0..rows).map(|_| blank_row(cols)).collect(),
            scrollback: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            style: CellStyle::default(),
            scrollback_limit: 5_000,
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.cols = cols;
        for row in self.screen.iter_mut() {
            row.resize(cols, Cell::default());
        }
        if rows > self.rows {
            for _ in self.rows..rows {
                self.screen.push(blank_row(cols));
            }
        } else if rows < self.rows {
            self.screen.truncate(rows);
        }
        self.rows = rows;
        if self.cursor_row >= rows { self.cursor_row = rows.saturating_sub(1); }
        if self.cursor_col >= cols { self.cursor_col = cols.saturating_sub(1); }
    }

    fn scroll_up(&mut self) {
        let row = self.screen.remove(0);
        self.scrollback.push(row);
        if self.scrollback.len() > self.scrollback_limit {
            let drop = self.scrollback.len() - self.scrollback_limit;
            self.scrollback.drain(..drop);
        }
        self.screen.push(blank_row(self.cols));
    }

    fn line_feed(&mut self) {
        if self.cursor_row + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cursor_row += 1;
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.line_feed();
        }
        let cell = &mut self.screen[self.cursor_row][self.cursor_col];
        cell.ch = ch;
        cell.style = self.style;
        self.cursor_col += 1;
    }

    /// All visible rows including scrollback (scrollback first, then screen).
    pub fn visible_rows(&self) -> impl Iterator<Item = &Row> {
        self.scrollback.iter().chain(self.screen.iter())
    }
}

fn blank_row(cols: usize) -> Row {
    vec![Cell::default(); cols]
}

pub struct Terminal {
    pub grid: Grid,
    parser: Parser,
}

impl Terminal {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self { grid: Grid::new(rows, cols), parser: Parser::new() }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.grid.resize(rows, cols);
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        let mut performer = GridPerform { grid: &mut self.grid };
        for b in bytes {
            self.parser.advance(&mut performer, *b);
        }
    }
}

struct GridPerform<'a> {
    grid: &'a mut Grid,
}

impl<'a> Perform for GridPerform<'a> {
    fn print(&mut self, c: char) {
        self.grid.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.grid.line_feed(),
            b'\r' => self.grid.cursor_col = 0,
            0x08 => {
                // Backspace: move cursor left (does not erase).
                if self.grid.cursor_col > 0 {
                    self.grid.cursor_col -= 1;
                }
            }
            0x07 => {} // BEL
            b'\t' => {
                let next = ((self.grid.cursor_col / 8) + 1) * 8;
                self.grid.cursor_col = next.min(self.grid.cols.saturating_sub(1));
            }
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}
    fn esc_dispatch(&mut self, _: &[u8], _: bool, _: u8) {}

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let first = || -> u16 {
            params.iter().next().and_then(|p| p.first().copied()).unwrap_or(0)
        };
        match action {
            'A' => {
                let n = first().max(1) as usize;
                self.grid.cursor_row = self.grid.cursor_row.saturating_sub(n);
            }
            'B' => {
                let n = first().max(1) as usize;
                self.grid.cursor_row =
                    (self.grid.cursor_row + n).min(self.grid.rows.saturating_sub(1));
            }
            'C' => {
                let n = first().max(1) as usize;
                self.grid.cursor_col =
                    (self.grid.cursor_col + n).min(self.grid.cols.saturating_sub(1));
            }
            'D' => {
                let n = first().max(1) as usize;
                self.grid.cursor_col = self.grid.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                let mut it = params.iter();
                let row = it.next().and_then(|p| p.first().copied()).unwrap_or(1);
                let col = it.next().and_then(|p| p.first().copied()).unwrap_or(1);
                let row = (row.max(1) as usize - 1).min(self.grid.rows.saturating_sub(1));
                let col = (col.max(1) as usize - 1).min(self.grid.cols.saturating_sub(1));
                self.grid.cursor_row = row;
                self.grid.cursor_col = col;
            }
            'J' => {
                let mode = first();
                match mode {
                    0 => {
                        // from cursor to end of screen
                        for c in self.grid.cursor_col..self.grid.cols {
                            self.grid.screen[self.grid.cursor_row][c] = Cell::default();
                        }
                        for r in (self.grid.cursor_row + 1)..self.grid.rows {
                            self.grid.screen[r] = blank_row(self.grid.cols);
                        }
                    }
                    1 => {
                        for r in 0..self.grid.cursor_row {
                            self.grid.screen[r] = blank_row(self.grid.cols);
                        }
                        for c in 0..=self.grid.cursor_col.min(self.grid.cols - 1) {
                            self.grid.screen[self.grid.cursor_row][c] = Cell::default();
                        }
                    }
                    2 | 3 => {
                        for r in 0..self.grid.rows {
                            self.grid.screen[r] = blank_row(self.grid.cols);
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = first();
                let row = self.grid.cursor_row;
                match mode {
                    0 => {
                        for c in self.grid.cursor_col..self.grid.cols {
                            self.grid.screen[row][c] = Cell::default();
                        }
                    }
                    1 => {
                        for c in 0..=self.grid.cursor_col.min(self.grid.cols - 1) {
                            self.grid.screen[row][c] = Cell::default();
                        }
                    }
                    2 => {
                        self.grid.screen[row] = blank_row(self.grid.cols);
                    }
                    _ => {}
                }
            }
            'P' => {
                let n = first().max(1) as usize;
                let row = self.grid.cursor_row;
                let col = self.grid.cursor_col;
                let line = &mut self.grid.screen[row];
                for _ in 0..n {
                    if col < line.len() {
                        line.remove(col);
                        line.push(Cell::default());
                    }
                }
            }
            'm' => apply_sgr(&mut self.grid.style, params),
            _ => {}
        }
    }
}

fn apply_sgr(style: &mut CellStyle, params: &Params) {
    let mut iter = params.iter();
    while let Some(slice) = iter.next() {
        let code = slice.first().copied().unwrap_or(0);
        match code {
            0 => *style = CellStyle::default(),
            1 => style.bold = true,
            22 => style.bold = false,
            30..=37 => style.fg = Color::Indexed((code - 30) as u8),
            39 => style.fg = Color::Default,
            40..=47 => style.bg = Color::Indexed((code - 40) as u8),
            49 => style.bg = Color::Default,
            90..=97 => style.fg = Color::Indexed((code - 90 + 8) as u8),
            100..=107 => style.bg = Color::Indexed((code - 100 + 8) as u8),
            38 => {
                if let Some(next) = iter.next() {
                    match next.first().copied() {
                        Some(5) => {
                            if let Some(idx) = iter.next().and_then(|s| s.first().copied()) {
                                style.fg = Color::Indexed(idx as u8);
                            }
                        }
                        Some(2) => {
                            let r = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                            let g = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                            let b = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                            style.fg = Color::Rgb(r, g, b);
                        }
                        _ => {}
                    }
                }
            }
            48 => {
                if let Some(next) = iter.next() {
                    match next.first().copied() {
                        Some(5) => {
                            if let Some(idx) = iter.next().and_then(|s| s.first().copied()) {
                                style.bg = Color::Indexed(idx as u8);
                            }
                        }
                        Some(2) => {
                            let r = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                            let g = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                            let b = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                            style.bg = Color::Rgb(r, g, b);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}
