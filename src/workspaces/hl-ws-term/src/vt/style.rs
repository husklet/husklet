use super::*;

impl Vt {
    pub(super) fn erase_display(&mut self, mode: u32) {
        let blank = self.blank();
        let (r, c) = (self.grid.cursor_row, self.grid.cursor_col);
        let (rows, cols) = (self.grid.rows(), self.grid.cols());
        match mode {
            0 => {
                // Cursor to end of screen.
                self.grid.clear_row_range(r, c, cols, blank);
                for row in (r + 1)..rows {
                    self.grid.clear_row_range(row, 0, cols, blank);
                }
            }
            1 => {
                // Start of screen to cursor.
                for row in 0..r {
                    self.grid.clear_row_range(row, 0, cols, blank);
                }
                self.grid.clear_row_range(r, 0, c + 1, blank);
            }
            _ => self.grid.clear_all(blank), // 2 / 3: whole screen
        }
    }

    pub(super) fn erase_line(&mut self, mode: u32) {
        let blank = self.blank();
        let (r, c) = (self.grid.cursor_row, self.grid.cursor_col);
        let cols = self.grid.cols();
        match mode {
            0 => self.grid.clear_row_range(r, c, cols, blank),
            1 => self.grid.clear_row_range(r, 0, c + 1, blank),
            _ => self.grid.clear_row_range(r, 0, cols, blank),
        }
    }

    pub(super) fn sgr(&mut self) {
        if self.params.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        while i < self.params.len() {
            let p = self.params[i];
            match p {
                0 => self.pen = Pen::default(),
                1 => self.pen.attrs.insert(Attrs::BOLD),
                2 => self.pen.attrs.insert(Attrs::DIM),
                3 => self.pen.attrs.insert(Attrs::ITALIC),
                4 => {
                    // `4:n` (colon-grouped) is an underline *style*: 0 = off, 1..=5 = single/double/curly/
                    // dotted/dashed. Consume the style sub-param so it isn't re-read as a standalone SGR
                    // code (a `4:3` undercurl must not also flip italic via a stray `3`). Plain `4`/`4;n`
                    // is just single underline.
                    i += self.apply_underline(i);
                }
                7 => self.pen.attrs.insert(Attrs::REVERSE),
                8 => self.pen.attrs.insert(Attrs::HIDDEN),
                9 => self.pen.attrs.insert(Attrs::STRIKE),
                22 => self.pen.attrs.remove(Attrs::BOLD | Attrs::DIM),
                23 => self.pen.attrs.remove(Attrs::ITALIC),
                24 => self.pen.attrs.remove(Attrs::UNDERLINE),
                27 => self.pen.attrs.remove(Attrs::REVERSE),
                29 => self.pen.attrs.remove(Attrs::STRIKE),
                30..=37 => self.pen.fg = Color::Indexed((p - 30) as u8),
                40..=47 => self.pen.bg = Color::Indexed((p - 40) as u8),
                90..=97 => self.pen.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((p - 100 + 8) as u8),
                39 => self.pen.fg = Color::Default,
                49 => self.pen.bg = Color::Default,
                38 | 48 => {
                    i += self.apply_extended_color(i, p == 38);
                }
                _ => {}
            }
            i += 1;
        }
    }

    pub(super) fn apply_underline(&mut self, index: usize) -> usize {
        if !self.param_colon.get(index).copied().unwrap_or(false) {
            self.pen.attrs.insert(Attrs::UNDERLINE);
            return 0;
        }
        if self.params.get(index + 1).copied().unwrap_or(1) == 0 {
            self.pen.attrs.remove(Attrs::UNDERLINE);
        } else {
            self.pen.attrs.insert(Attrs::UNDERLINE);
        }
        1
    }

    pub(super) fn apply_extended_color(&mut self, index: usize, foreground: bool) -> usize {
        let Some(kind) = self.params.get(index + 1).copied() else {
            return 0;
        };
        match kind {
            5 => {
                if let Some(value) = self.params.get(index + 2).copied() {
                    self.pen.set_color(foreground, Color::Indexed(value as u8));
                }
                2
            }
            2 => {
                let offset = if self.saw_colon { 3 } else { 2 };
                let channel = |at| self.params.get(index + at).copied().unwrap_or(0) as u8;
                self.pen.set_color(
                    foreground,
                    Color::Rgb(channel(offset), channel(offset + 1), channel(offset + 2)),
                );
                offset + 2
            }
            _ => 0,
        }
    }

    // ---- OSC ------------------------------------------------------------------------------------
    pub(super) fn osc(&mut self, b: u8) {
        match b {
            0x07 => {
                self.finish_osc();
                self.state = State::Ground;
            }
            0x1b => {
                // Possible ST (ESC \) — the next byte (\) ends it; approximate by finishing now and
                // letting the trailing byte be handled in Ground (it'll be a no-op backslash char).
                self.finish_osc();
                self.state = State::Esc;
            }
            _ => {
                if self.osc.len() < 1024 {
                    self.osc.push(b);
                }
            }
        }
    }

    pub(super) fn finish_osc(&mut self) {
        // OSC 0;title (icon+title) or 2;title (title) set the window title.
        if let Ok(s) = std::str::from_utf8(&self.osc) {
            if let Some((num, rest)) = s.split_once(';') {
                match num {
                    // OSC 0 (icon+title) / 2 (title) set the window title.
                    "0" | "2" => self.title = rest.to_string(),
                    // OSC 7 reports the shell's cwd as a `file://host/path` URI.
                    "7" => {
                        if let Some(path) = crate::session::WorkingDirectory::from_osc7(rest) {
                            self.cwd = Some(path.into_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        self.osc.clear();
    }
}
