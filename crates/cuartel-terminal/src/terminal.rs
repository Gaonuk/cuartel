use gpui::SharedString;

pub struct TerminalBuffer {
    lines: Vec<SharedString>,
    max_lines: usize,
}

impl TerminalBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: Vec::new(),
            max_lines,
        }
    }

    pub fn push_line(&mut self, line: &str) {
        self.lines.push(SharedString::from(line.to_string()));
        if self.lines.len() > self.max_lines {
            self.lines.remove(0);
        }
    }

    pub fn push_text(&mut self, text: &str) {
        for line in text.lines() {
            self.push_line(line);
        }
    }

    pub fn lines(&self) -> &[SharedString] {
        &self.lines
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}
