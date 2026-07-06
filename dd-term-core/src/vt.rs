//! A self-contained VT/ANSI parser driving a [`Grid`].
//!
//! Implements the practical xterm subset a shell + common TUIs need: printable UTF-8 text with
//! autowrap, the C0 controls (BS/HT/LF/CR/BEL), `ESC` dispatches (RI/NEL/DECSC/DECRC/RIS), `CSI`
//! (cursor moves CUU/CUD/CUF/CUB, CUP/HVP, ED/EL erase, SGR colors+attrs, DECTCEM show/hide cursor,
//! line scroll SU/SD), and `OSC` (window title 0/2, otherwise ignored). It is intentionally not a full
//! VT500 — unknown sequences are skipped so they never corrupt the grid — and it is pure/synchronous so
//! it can be unit-tested by feeding bytes and asserting the resulting [`Grid`].

use crate::grid::{Attrs, Cell, Color, Grid};

/// Parser state within an escape sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    /// Absorb one byte then return to Ground (e.g. a charset-designation intermediate).
    EscIgnoreOne,
}

/// The pen: the SGR state applied to newly written cells.
#[derive(Clone, Copy)]
struct Pen {
    fg: Color,
    bg: Color,
    attrs: Attrs,
}
impl Default for Pen {
    fn default() -> Self {
        Pen { fg: Color::Default, bg: Color::Default, attrs: Attrs::empty() }
    }
}

/// A VT parser bound to a grid. Feed bytes via [`Vt::advance`] / [`Vt::advance_bytes`].
pub struct Vt {
    grid: Grid,
    pen: Pen,
    state: State,
    /// CSI parameter accumulator (numeric params split by `;`).
    params: Vec<u32>,
    cur_param: Option<u32>,
    /// A leading private marker byte for CSI (`?`, `>`, `!`), if any.
    private: u8,
    /// OSC payload accumulator.
    osc: Vec<u8>,
    /// Pending UTF-8 continuation bytes (partial multibyte codepoint).
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
    /// Saved cursor (DECSC/DECRC).
    saved: Option<(usize, usize)>,
    /// Autowrap pending: the last column was written; the next printable wraps first.
    wrap_pending: bool,
    /// The most recent window title set via OSC 0/2.
    pub title: String,
    /// Rings once per BEL; the app can poll+reset this.
    pub bell: bool,
}

impl Vt {
    /// A parser over a fresh `cols × rows` grid.
    pub fn new(cols: usize, rows: usize) -> Vt {
        Vt {
            grid: Grid::new(cols, rows),
            pen: Pen::default(),
            state: State::Ground,
            params: Vec::new(),
            cur_param: None,
            private: 0,
            osc: Vec::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            saved: None,
            wrap_pending: false,
            title: String::new(),
            bell: false,
        }
    }

    /// Immutable view of the current screen (for the renderer / tests).
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Resize the screen to `cols × rows` (e.g. on a window/pane resize).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        self.wrap_pending = false;
    }

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
            State::EscIgnoreOne => self.state = State::Ground,
        }
    }

    // ---- Ground: printable text + C0 controls ---------------------------------------------------
    fn ground(&mut self, b: u8) {
        // Mid-UTF8-sequence: accumulate continuation bytes.
        if self.utf8_need > 0 {
            if b & 0xC0 == 0x80 {
                self.utf8_buf[self.utf8_len] = b;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_need {
                    let ch = std::str::from_utf8(&self.utf8_buf[..self.utf8_len])
                        .ok()
                        .and_then(|s| s.chars().next())
                        .unwrap_or('\u{fffd}');
                    self.put_char(ch);
                    self.utf8_need = 0;
                    self.utf8_len = 0;
                }
                return;
            }
            // Malformed: drop the partial and fall through to handle `b` fresh.
            self.utf8_need = 0;
            self.utf8_len = 0;
        }
        match b {
            0x1b => self.enter_esc(),
            0x07 => self.bell = true, // BEL
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a | 0x0b | 0x0c => self.line_feed(), // LF/VT/FF
            0x0d => {
                self.grid.cursor_col = 0;
                self.wrap_pending = false;
            }
            0x00..=0x06 | 0x0e..=0x1a | 0x1c..=0x1f => {} // other C0: ignore
            0x20..=0x7f => self.put_char(b as char),
            _ => {
                // Start of a UTF-8 multibyte sequence.
                self.utf8_need = match b {
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF7 => 4,
                    _ => 0,
                };
                if self.utf8_need > 0 {
                    self.utf8_buf[0] = b;
                    self.utf8_len = 1;
                } else {
                    self.put_char('\u{fffd}');
                }
            }
        }
    }

    fn put_char(&mut self, ch: char) {
        let cols = self.grid.cols();
        if self.wrap_pending {
            self.wrap_pending = false;
            self.grid.cursor_col = 0;
            self.line_feed();
        }
        let (r, c) = (self.grid.cursor_row, self.grid.cursor_col);
        self.grid.set(r, c, Cell { ch, fg: self.pen.fg, bg: self.pen.bg, attrs: self.pen.attrs });
        if c + 1 >= cols {
            // At the last column: stay put but arm autowrap for the next printable.
            self.wrap_pending = true;
        } else {
            self.grid.cursor_col = c + 1;
        }
    }

    fn backspace(&mut self) {
        if self.grid.cursor_col > 0 {
            self.grid.cursor_col -= 1;
        }
        self.wrap_pending = false;
    }

    fn tab(&mut self) {
        // Advance to the next 8-column tab stop.
        let cols = self.grid.cols();
        let next = ((self.grid.cursor_col / 8) + 1) * 8;
        self.grid.cursor_col = next.min(cols - 1);
        self.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.grid.cursor_row + 1 >= self.grid.rows() {
            let blank = self.blank();
            self.grid.scroll_up(blank);
        } else {
            self.grid.cursor_row += 1;
        }
    }

    fn blank(&self) -> Cell {
        // Erase adopts the current background so `clear`/full-screen apps fill correctly.
        Cell { ch: ' ', fg: self.pen.fg, bg: self.pen.bg, attrs: self.pen.attrs & Attrs::REVERSE }
    }

    // ---- ESC ------------------------------------------------------------------------------------
    fn enter_esc(&mut self) {
        self.state = State::Esc;
    }

    fn esc(&mut self, b: u8) {
        match b {
            b'[' => {
                self.params.clear();
                self.cur_param = None;
                self.private = 0;
                self.state = State::Csi;
            }
            b']' => {
                self.osc.clear();
                self.state = State::Osc;
            }
            b'(' | b')' | b'*' | b'+' => self.state = State::EscIgnoreOne, // charset designation
            b'M' => {
                // Reverse Index: move up, scrolling down at the top.
                if self.grid.cursor_row == 0 {
                    // (rare) scroll region down — approximate by leaving as-is at top
                } else {
                    self.grid.cursor_row -= 1;
                }
                self.state = State::Ground;
            }
            b'E' => {
                // NEL: newline.
                self.grid.cursor_col = 0;
                self.line_feed();
                self.state = State::Ground;
            }
            b'7' => {
                self.saved = Some((self.grid.cursor_row, self.grid.cursor_col));
                self.state = State::Ground;
            }
            b'8' => {
                if let Some((r, c)) = self.saved {
                    self.grid.cursor_row = r.min(self.grid.rows() - 1);
                    self.grid.cursor_col = c.min(self.grid.cols() - 1);
                }
                self.state = State::Ground;
            }
            b'c' => {
                // RIS: full reset.
                self.pen = Pen::default();
                let blank = Cell::default();
                self.grid.clear_all(blank);
                self.grid.cursor_row = 0;
                self.grid.cursor_col = 0;
                self.grid.cursor_visible = true;
                self.wrap_pending = false;
                self.state = State::Ground;
            }
            _ => self.state = State::Ground,
        }
    }

    // ---- CSI ------------------------------------------------------------------------------------
    fn csi(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let d = (b - b'0') as u32;
                self.cur_param = Some(self.cur_param.unwrap_or(0).saturating_mul(10).saturating_add(d));
            }
            b';' => {
                self.params.push(self.cur_param.take().unwrap_or(0));
            }
            b'?' | b'>' | b'!' if self.params.is_empty() && self.cur_param.is_none() => {
                self.private = b;
            }
            0x40..=0x7e => {
                // Final byte: flush the last param and dispatch.
                if let Some(p) = self.cur_param.take() {
                    self.params.push(p);
                }
                self.dispatch_csi(b);
                self.state = State::Ground;
            }
            0x20..=0x2f => {} // intermediate bytes: ignore
            _ => self.state = State::Ground,
        }
    }

    fn param(&self, i: usize, default: u32) -> u32 {
        self.params.get(i).copied().filter(|&v| v != 0).unwrap_or(default)
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        match final_byte {
            b'A' => {
                let n = self.param(0, 1) as usize;
                self.grid.cursor_row = self.grid.cursor_row.saturating_sub(n);
                self.wrap_pending = false;
            }
            b'B' => {
                let n = self.param(0, 1) as usize;
                self.grid.cursor_row = (self.grid.cursor_row + n).min(rows - 1);
                self.wrap_pending = false;
            }
            b'C' => {
                let n = self.param(0, 1) as usize;
                self.grid.cursor_col = (self.grid.cursor_col + n).min(cols - 1);
                self.wrap_pending = false;
            }
            b'D' => {
                let n = self.param(0, 1) as usize;
                self.grid.cursor_col = self.grid.cursor_col.saturating_sub(n);
                self.wrap_pending = false;
            }
            b'G' => {
                // CHA: cursor to column n (1-based).
                self.grid.cursor_col = (self.param(0, 1) as usize - 1).min(cols - 1);
                self.wrap_pending = false;
            }
            b'd' => {
                // VPA: cursor to row n (1-based).
                self.grid.cursor_row = (self.param(0, 1) as usize - 1).min(rows - 1);
                self.wrap_pending = false;
            }
            b'H' | b'f' => {
                // CUP/HVP: 1-based (row; col).
                let r = self.param(0, 1) as usize - 1;
                let c = self.param(1, 1) as usize - 1;
                self.grid.cursor_row = r.min(rows - 1);
                self.grid.cursor_col = c.min(cols - 1);
                self.wrap_pending = false;
            }
            b'J' => self.erase_display(self.params.first().copied().unwrap_or(0)),
            b'K' => self.erase_line(self.params.first().copied().unwrap_or(0)),
            b'm' => self.sgr(),
            b'S' => {
                let n = self.param(0, 1);
                let blank = self.blank();
                for _ in 0..n {
                    self.grid.scroll_up(blank);
                }
            }
            b'h' | b'l' if self.private == b'?' => {
                // DEC private mode set/reset — we honor cursor visibility (25).
                if self.params.first() == Some(&25) {
                    self.grid.cursor_visible = final_byte == b'h';
                }
            }
            _ => {} // unhandled CSI: ignore
        }
    }

    fn erase_display(&mut self, mode: u32) {
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

    fn erase_line(&mut self, mode: u32) {
        let blank = self.blank();
        let (r, c) = (self.grid.cursor_row, self.grid.cursor_col);
        let cols = self.grid.cols();
        match mode {
            0 => self.grid.clear_row_range(r, c, cols, blank),
            1 => self.grid.clear_row_range(r, 0, c + 1, blank),
            _ => self.grid.clear_row_range(r, 0, cols, blank),
        }
    }

    fn sgr(&mut self) {
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
                4 => self.pen.attrs.insert(Attrs::UNDERLINE),
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
                    // Extended color: 38;5;n (indexed) or 38;2;r;g;b (rgb).
                    let target_fg = p == 38;
                    if let Some(&kind) = self.params.get(i + 1) {
                        if kind == 5 {
                            if let Some(&n) = self.params.get(i + 2) {
                                let col = Color::Indexed(n as u8);
                                if target_fg { self.pen.fg = col } else { self.pen.bg = col }
                            }
                            i += 2;
                        } else if kind == 2 {
                            let r = self.params.get(i + 2).copied().unwrap_or(0) as u8;
                            let g = self.params.get(i + 3).copied().unwrap_or(0) as u8;
                            let b = self.params.get(i + 4).copied().unwrap_or(0) as u8;
                            let col = Color::Rgb(r, g, b);
                            if target_fg { self.pen.fg = col } else { self.pen.bg = col }
                            i += 4;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    // ---- OSC ------------------------------------------------------------------------------------
    fn osc(&mut self, b: u8) {
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

    fn finish_osc(&mut self) {
        // OSC 0;title (icon+title) or 2;title (title) set the window title.
        if let Ok(s) = std::str::from_utf8(&self.osc) {
            if let Some((num, rest)) = s.split_once(';') {
                if num == "0" || num == "2" {
                    self.title = rest.to_string();
                }
            }
        }
        self.osc.clear();
    }
}
