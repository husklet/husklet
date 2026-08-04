use super::*;

impl Vt {
    /// Feed a run of bytes.
    pub fn advance_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.advance(b);
        }
    }

    /// Feed one byte.
    pub fn advance(&mut self, b: u8) {
        match self.state {
            State::Ground => self.ground(b),
            State::Esc => self.esc(b),
            State::Csi => self.csi(b),
            State::Osc => self.osc(b),
            State::EscCharset => {
                // Byte designates a charset into G0: `0` = DEC special graphics, else ASCII.
                self.charset_g0_dec = b == b'0';
                self.state = State::Ground;
            }
            State::EscIgnoreOne => self.state = State::Ground,
        }
    }

    // ---- Ground: printable text + C0 controls ---------------------------------------------------
    pub(super) fn ground(&mut self, b: u8) {
        if self.resume_utf8(b) {
            return;
        }
        match b {
            0x1b => self.enter_esc(),
            0x07 => self.bell = true, // BEL
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a..=0x0c => self.line_feed(), // LF/VT/FF
            0x0d => {
                self.grid.cursor_col = 0;
                self.wrap_pending = false;
            }
            0x00..=0x06 | 0x0e..=0x1a | 0x1c..=0x1f => {} // other C0 (incl. SO/SI): ignore
            0x7f => {}                                    // DEL: ignored in ground (not a glyph)
            0x20..=0x7e => self.put_char(b as char),
            _ => self.start_utf8(b),
        }
    }

    pub(super) fn resume_utf8(&mut self, byte: u8) -> bool {
        if self.utf8_need == 0 {
            return false;
        }
        if byte & 0xC0 != 0x80 {
            self.utf8_need = 0;
            self.utf8_len = 0;
            return false;
        }
        self.utf8_buf[self.utf8_len] = byte;
        self.utf8_len += 1;
        if self.utf8_len != self.utf8_need {
            return true;
        }
        let ch = std::str::from_utf8(&self.utf8_buf[..self.utf8_len])
            .ok()
            .and_then(|text| text.chars().next())
            .unwrap_or('\u{fffd}');
        self.utf8_need = 0;
        self.utf8_len = 0;
        self.put_char(ch);
        true
    }

    pub(super) fn start_utf8(&mut self, byte: u8) {
        self.utf8_need = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => {
                self.put_char('\u{fffd}');
                return;
            }
        };
        self.utf8_buf[0] = byte;
        self.utf8_len = 1;
    }

    pub(super) fn put_char(&mut self, ch: char) {
        let ch = self.translate(ch);
        let cols = self.grid.cols();
        if self.wrap_pending {
            self.wrap_pending = false;
            self.grid.cursor_col = 0;
            self.line_feed();
        }
        let (r, c) = (self.grid.cursor_row, self.grid.cursor_col);
        self.grid.set(
            r,
            c,
            Cell {
                ch,
                fg: self.pen.fg,
                bg: self.pen.bg,
                attrs: self.pen.attrs,
            },
        );
        if c + 1 >= cols {
            // At the last column: with autowrap (DECAWM) arm a wrap for the next printable; with it off
            // the cursor sticks at the last column and further chars overwrite it (no scroll).
            if self.autowrap {
                self.wrap_pending = true;
            }
        } else {
            self.grid.cursor_col = c + 1;
        }
    }

    /// Translate through the active G0 character set before storing a grid cell.
    pub(super) fn translate(&self, ch: char) -> char {
        if !self.charset_g0_dec {
            return ch;
        }
        match ch {
            '_' => '\u{00a0}',
            '`' => '\u{25c6}',
            'a' => '\u{2592}',
            'b' => '\u{2409}',
            'c' => '\u{240c}',
            'd' => '\u{240d}',
            'e' => '\u{240a}',
            'f' => '\u{00b0}',
            'g' => '\u{00b1}',
            'h' => '\u{2424}',
            'i' => '\u{240b}',
            'j' => '\u{2518}',
            'k' => '\u{2510}',
            'l' => '\u{250c}',
            'm' => '\u{2514}',
            'n' => '\u{253c}',
            'o' => '\u{23ba}',
            'p' => '\u{23bb}',
            'q' => '\u{2500}',
            'r' => '\u{23bc}',
            's' => '\u{23bd}',
            't' => '\u{251c}',
            'u' => '\u{2524}',
            'v' => '\u{2534}',
            'w' => '\u{252c}',
            'x' => '\u{2502}',
            'y' => '\u{2264}',
            'z' => '\u{2265}',
            '{' => '\u{03c0}',
            '|' => '\u{2260}',
            '}' => '\u{00a3}',
            '~' => '\u{00b7}',
            other => other,
        }
    }

    pub(super) fn backspace(&mut self) {
        if self.grid.cursor_col > 0 {
            self.grid.cursor_col -= 1;
        }
        self.wrap_pending = false;
    }

    pub(super) fn tab(&mut self) {
        // Advance to the next 8-column tab stop.
        let cols = self.grid.cols();
        let next = ((self.grid.cursor_col / 8) + 1) * 8;
        self.grid.cursor_col = next.min(cols - 1);
        self.wrap_pending = false;
    }

    pub(super) fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.grid.cursor_row == self.scroll_bot {
            // At the bottom margin: scroll the region up (a pinned line outside the region stays put).
            let blank = self.blank();
            self.grid.scroll_region_up(self.scroll_top, self.scroll_bot, blank);
        } else if self.grid.cursor_row + 1 < self.grid.rows() {
            self.grid.cursor_row += 1;
        }
    }

    /// Save cursor + pen (DECSC / SCO save).
    pub(super) fn save_cursor(&mut self) {
        self.saved = Some((self.grid.cursor_row, self.grid.cursor_col, self.pen));
    }

    /// Restore cursor + pen (DECRC / SCO restore).
    pub(super) fn restore_cursor(&mut self) {
        if let Some((r, c, p)) = self.saved {
            self.grid.cursor_row = r.min(self.grid.rows() - 1);
            self.grid.cursor_col = c.min(self.grid.cols() - 1);
            self.pen = p;
        }
        self.wrap_pending = false;
    }

    /// Switch to the alternate screen buffer (`?1049h`/`?47h`/`?1047h`). `save` also stashes the cursor
    /// (the `1049` variant). The alt buffer starts blank; the primary is stored for restore.
    pub(super) fn enter_alt(&mut self, save: bool) {
        if self.alt_active {
            return;
        }
        if save {
            self.save_cursor();
        }
        let (cols, rows) = (self.grid.cols(), self.grid.rows());
        let mut alt = Grid::new(cols, rows);
        alt.cursor_visible = self.grid.cursor_visible;
        let primary = std::mem::replace(&mut self.grid, alt);
        self.stored = Some(primary);
        self.alt_active = true;
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
    }

    /// Return to the primary screen (`?1049l`/`?47l`/`?1047l`), restoring its saved contents. `restore`
    /// also restores the saved cursor (the `1049` variant), returning you to the pre-launch prompt.
    pub(super) fn leave_alt(&mut self, restore: bool) {
        if !self.alt_active {
            return;
        }
        if let Some(primary) = self.stored.take() {
            self.grid = primary;
        }
        self.alt_active = false;
        if restore {
            self.restore_cursor();
        }
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bot = self.grid.rows() - 1;
    }

    pub(super) fn blank(&self) -> Cell {
        // Erase adopts the current background so `clear`/full-screen apps fill correctly.
        Cell {
            ch: ' ',
            fg: self.pen.fg,
            bg: self.pen.bg,
            attrs: self.pen.attrs & Attrs::REVERSE,
        }
    }

    // ---- ESC ------------------------------------------------------------------------------------
}
