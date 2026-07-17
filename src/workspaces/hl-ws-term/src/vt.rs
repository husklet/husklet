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
    /// `ESC (` / `ESC )` … : the next byte designates a charset into G0/G1.
    EscCharset,
    /// Absorb one byte then return to Ground (an unhandled intermediate, e.g. `ESC #`).
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
        Pen {
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::empty(),
        }
    }
}

/// A VT parser bound to a grid. Feed bytes via [`Vt::advance`] / [`Vt::advance_bytes`].
pub struct Vt {
    grid: Grid,
    /// The inactive screen buffer (primary while the alt screen is active, and vice-versa). Swapped in
    /// on `CSI ?1049h/l` etc. so full-screen apps (vim/htop/less) restore the shell screen on exit.
    stored: Option<Grid>,
    alt_active: bool,
    pen: Pen,
    state: State,
    /// CSI parameter accumulator (numeric params split by `;` or `:`).
    params: Vec<u32>,
    cur_param: Option<u32>,
    /// True if any sub-parameter in the current CSI was `:`-separated (ISO-8613-6 SGR).
    saw_colon: bool,
    /// Parallel to `params`: whether each param was terminated by a `:` (vs `;`/final). Lets SGR tell a
    /// colon-grouped code (`4:3` undercurl, `4:2` double, …) from a semicolon list (`4;3` = underline
    /// then italic).
    param_colon: Vec<bool>,
    /// A leading private marker byte for CSI (`?`, `>`, `!`), if any.
    private: u8,
    /// OSC payload accumulator.
    osc: Vec<u8>,
    /// Pending UTF-8 continuation bytes (partial multibyte codepoint).
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
    /// Saved cursor + pen (DECSC/DECRC `ESC 7`/`ESC 8` and SCO `CSI s`/`CSI u`).
    saved: Option<(usize, usize, Pen)>,
    /// Scroll region (DECSTBM), inclusive 0-based rows. Defaults to the whole screen.
    scroll_top: usize,
    scroll_bot: usize,
    /// DECAWM autowrap enabled (`CSI ?7h/l`); default on.
    autowrap: bool,
    /// G0 is the DEC Special Graphics (line-drawing) charset (`ESC ( 0`) vs ASCII (`ESC ( B`).
    charset_g0_dec: bool,
    /// Autowrap pending: the last column was written; the next printable wraps first.
    wrap_pending: bool,
    /// The most recent window title set via OSC 0/2.
    pub title: String,
    /// The most recent working directory reported via OSC 7 (`file://host/path`), decoded to a plain
    /// path. Shells emit this on every prompt; the session layer persists it so a reopened pane restores
    /// its cwd. Was previously dropped on the floor, so cwd never tracked in the GPU (non-VTE) path.
    pub cwd: Option<String>,
    /// Rings once per BEL; the app can poll+reset this.
    pub bell: bool,
}

impl Vt {
    /// A parser over a fresh `cols × rows` grid.
    pub fn new(cols: usize, rows: usize) -> Vt {
        let g = Grid::new(cols, rows);
        let bot = g.rows() - 1;
        Vt {
            grid: g,
            stored: None,
            alt_active: false,
            pen: Pen::default(),
            state: State::Ground,
            params: Vec::new(),
            cur_param: None,
            saw_colon: false,
            param_colon: Vec::new(),
            private: 0,
            osc: Vec::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            saved: None,
            scroll_top: 0,
            scroll_bot: bot,
            autowrap: true,
            charset_g0_dec: false,
            wrap_pending: false,
            title: String::new(),
            cwd: None,
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
        if let Some(s) = self.stored.as_mut() {
            s.resize(cols, rows);
        }
        // A resize resets the scroll region to the full (new) screen, matching xterm.
        self.scroll_top = 0;
        self.scroll_bot = self.grid.rows() - 1;
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
            State::EscCharset => {
                // Byte designates a charset into G0: `0` = DEC special graphics, else ASCII.
                self.charset_g0_dec = b == b'0';
                self.state = State::Ground;
            }
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
            0x00..=0x06 | 0x0e..=0x1a | 0x1c..=0x1f => {} // other C0 (incl. SO/SI): ignore
            0x7f => {}                                    // DEL: ignored in ground (not a glyph)
            0x20..=0x7e => self.put_char(b as char),
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
        let ch = if self.charset_g0_dec {
            dec_graphic(ch)
        } else {
            ch
        };
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
        if self.grid.cursor_row == self.scroll_bot {
            // At the bottom margin: scroll the region up (a pinned line outside the region stays put).
            let blank = self.blank();
            self.grid
                .scroll_region_up(self.scroll_top, self.scroll_bot, blank);
        } else if self.grid.cursor_row + 1 < self.grid.rows() {
            self.grid.cursor_row += 1;
        }
    }

    /// Save cursor + pen (DECSC / SCO save).
    fn save_cursor(&mut self) {
        self.saved = Some((self.grid.cursor_row, self.grid.cursor_col, self.pen));
    }

    /// Restore cursor + pen (DECRC / SCO restore).
    fn restore_cursor(&mut self) {
        if let Some((r, c, p)) = self.saved {
            self.grid.cursor_row = r.min(self.grid.rows() - 1);
            self.grid.cursor_col = c.min(self.grid.cols() - 1);
            self.pen = p;
        }
        self.wrap_pending = false;
    }

    /// Switch to the alternate screen buffer (`?1049h`/`?47h`/`?1047h`). `save` also stashes the cursor
    /// (the `1049` variant). The alt buffer starts blank; the primary is stored for restore.
    fn enter_alt(&mut self, save: bool) {
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
    fn leave_alt(&mut self, restore: bool) {
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

    fn blank(&self) -> Cell {
        // Erase adopts the current background so `clear`/full-screen apps fill correctly.
        Cell {
            ch: ' ',
            fg: self.pen.fg,
            bg: self.pen.bg,
            attrs: self.pen.attrs & Attrs::REVERSE,
        }
    }

    // ---- ESC ------------------------------------------------------------------------------------
    fn enter_esc(&mut self) {
        self.state = State::Esc;
    }

    fn esc(&mut self, b: u8) {
        match b {
            b'[' => {
                self.params.clear();
                self.param_colon.clear();
                self.cur_param = None;
                self.private = 0;
                self.saw_colon = false;
                self.state = State::Csi;
            }
            b']' => {
                self.osc.clear();
                self.state = State::Osc;
            }
            b'(' => self.state = State::EscCharset, // designate G0 charset from the next byte
            b')' | b'*' | b'+' | b'#' => self.state = State::EscIgnoreOne, // G1..G3 / DECALN: absorb one
            b'M' => {
                // Reverse Index: move up, scrolling the region DOWN when at the top margin.
                if self.grid.cursor_row == self.scroll_top {
                    let blank = self.blank();
                    self.grid
                        .scroll_region_down(self.scroll_top, self.scroll_bot, blank);
                } else if self.grid.cursor_row > 0 {
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
                self.save_cursor();
                self.state = State::Ground;
            }
            b'8' => {
                self.restore_cursor();
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
                self.cur_param = Some(
                    self.cur_param
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(d),
                );
            }
            b';' => self.push_param(false),
            b':' => {
                // ISO-8613-6 sub-parameter separator (e.g. `38:2::r:g:b`). Treat like `;` so the CSI is
                // not aborted (which used to splatter the tail as literal text); remember colon form so
                // the extended-color parser can skip the colorspace slot.
                self.saw_colon = true;
                self.push_param(true);
            }
            b'?' | b'>' | b'!' if self.params.is_empty() && self.cur_param.is_none() => {
                self.private = b;
            }
            0x40..=0x7e => {
                // Final byte: flush the last param and dispatch.
                if self.cur_param.is_some() {
                    self.push_param(false);
                }
                self.dispatch_csi(b);
                self.state = State::Ground;
            }
            0x20..=0x2f => {} // intermediate bytes: ignore
            _ => self.state = State::Ground,
        }
    }

    /// Flush `cur_param` into `params`, capped so a pathological `CSI 1;1;1;…` can't grow unbounded.
    /// `colon` records whether the separator that terminated this param was a `:` (ISO-8613-6 group).
    fn push_param(&mut self, colon: bool) {
        let v = self.cur_param.take().unwrap_or(0);
        if self.params.len() < 64 {
            self.params.push(v);
            self.param_colon.push(colon);
        }
    }

    fn param(&self, i: usize, default: u32) -> u32 {
        self.params
            .get(i)
            .copied()
            .filter(|&v| v != 0)
            .unwrap_or(default)
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let r = self.grid.cursor_row;
        let c = self.grid.cursor_col;
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
            b'X' => {
                // ECH: erase n cells at the cursor without moving it.
                let n = self.param(0, 1) as usize;
                let blank = self.blank();
                self.grid.clear_row_range(r, c, c + n, blank);
            }
            b'@' => {
                // ICH: insert n blank cells, shifting the rest of the line right.
                let n = self.param(0, 1) as usize;
                let blank = self.blank();
                self.grid.insert_cells(r, c, n, blank);
            }
            b'P' => {
                // DCH: delete n cells, shifting the rest of the line left.
                let n = self.param(0, 1) as usize;
                let blank = self.blank();
                self.grid.delete_cells(r, c, n, blank);
            }
            b'L' => {
                // IL: insert n blank lines at the cursor row within the scroll region.
                if r >= self.scroll_top && r <= self.scroll_bot {
                    let n = self.param(0, 1);
                    let blank = self.blank();
                    for _ in 0..n {
                        self.grid.scroll_region_down(r, self.scroll_bot, blank);
                    }
                }
            }
            b'M' => {
                // DL: delete n lines at the cursor row within the scroll region.
                if r >= self.scroll_top && r <= self.scroll_bot {
                    let n = self.param(0, 1);
                    let blank = self.blank();
                    for _ in 0..n {
                        self.grid.scroll_region_up(r, self.scroll_bot, blank);
                    }
                }
            }
            b'm' => self.sgr(),
            b'r' if self.private == 0 => {
                // DECSTBM: set the scroll region (top;bottom, 1-based). No params = full screen.
                let top = self.param(0, 1) as usize - 1;
                let bot = self
                    .params
                    .get(1)
                    .copied()
                    .filter(|&v| v != 0)
                    .map(|v| v as usize - 1)
                    .unwrap_or(rows - 1);
                if top < bot && bot < rows {
                    self.scroll_top = top;
                    self.scroll_bot = bot;
                } else {
                    self.scroll_top = 0;
                    self.scroll_bot = rows - 1;
                }
                self.grid.cursor_row = 0;
                self.grid.cursor_col = 0;
                self.wrap_pending = false;
            }
            b's' if self.private == 0 => self.save_cursor(), // SCO save cursor
            b'u' if self.private == 0 => self.restore_cursor(), // SCO restore cursor
            b'S' => {
                // SU: scroll the region up n lines.
                let n = self.param(0, 1);
                let blank = self.blank();
                for _ in 0..n {
                    self.grid
                        .scroll_region_up(self.scroll_top, self.scroll_bot, blank);
                }
            }
            b'T' => {
                // SD: scroll the region down n lines.
                let n = self.param(0, 1);
                let blank = self.blank();
                for _ in 0..n {
                    self.grid
                        .scroll_region_down(self.scroll_top, self.scroll_bot, blank);
                }
            }
            b'h' | b'l' if self.private == b'?' => {
                let set = final_byte == b'h';
                match self.params.first().copied().unwrap_or(0) {
                    25 => self.grid.cursor_visible = set, // DECTCEM show/hide cursor
                    7 => self.autowrap = set,             // DECAWM autowrap
                    1049 => {
                        if set {
                            self.enter_alt(true);
                        } else {
                            self.leave_alt(true);
                        }
                    }
                    47 | 1047 => {
                        if set {
                            self.enter_alt(false);
                        } else {
                            self.leave_alt(false);
                        }
                    }
                    1048 => {
                        if set {
                            self.save_cursor();
                        } else {
                            self.restore_cursor();
                        }
                    }
                    _ => {} // mouse/bracketed-paste/cursor-key modes are input-side: ignore on output
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
                4 => {
                    // `4:n` (colon-grouped) is an underline *style*: 0 = off, 1..=5 = single/double/curly/
                    // dotted/dashed. Consume the style sub-param so it isn't re-read as a standalone SGR
                    // code (a `4:3` undercurl must not also flip italic via a stray `3`). Plain `4`/`4;n`
                    // is just single underline.
                    if self.param_colon.get(i).copied().unwrap_or(false) {
                        let style = self.params.get(i + 1).copied().unwrap_or(1);
                        if style == 0 {
                            self.pen.attrs.remove(Attrs::UNDERLINE);
                        } else {
                            self.pen.attrs.insert(Attrs::UNDERLINE);
                        }
                        i += 1;
                    } else {
                        self.pen.attrs.insert(Attrs::UNDERLINE);
                    }
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
                    // Extended color: 38;5;n (indexed) or 38;2;r;g;b (rgb).
                    let target_fg = p == 38;
                    if let Some(&kind) = self.params.get(i + 1) {
                        if kind == 5 {
                            if let Some(&n) = self.params.get(i + 2) {
                                let col = Color::Indexed(n as u8);
                                if target_fg {
                                    self.pen.fg = col
                                } else {
                                    self.pen.bg = col
                                }
                            }
                            i += 2;
                        } else if kind == 2 {
                            // Semicolon form is `38;2;r;g;b`; the ISO colon form is `38:2:<cs>:r:g:b`
                            // with an extra colorspace-id slot to skip.
                            let off = if self.saw_colon { 3 } else { 2 };
                            let r = self.params.get(i + off).copied().unwrap_or(0) as u8;
                            let g = self.params.get(i + off + 1).copied().unwrap_or(0) as u8;
                            let b = self.params.get(i + off + 2).copied().unwrap_or(0) as u8;
                            let col = Color::Rgb(r, g, b);
                            if target_fg {
                                self.pen.fg = col
                            } else {
                                self.pen.bg = col
                            }
                            i += off + 2;
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
                match num {
                    // OSC 0 (icon+title) / 2 (title) set the window title.
                    "0" | "2" => self.title = rest.to_string(),
                    // OSC 7 reports the shell's cwd as a `file://host/path` URI.
                    "7" => {
                        if let Some(path) = crate::session::cwd_from_uri(rest) {
                            self.cwd = Some(path);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.osc.clear();
    }
}

/// Translate a byte through the DEC Special Graphics charset (`ESC ( 0`) — the box-drawing set that
/// `tmux`/`mc`/ncurses table UIs use. The complete VT100 set covers `0x5f..=0x7e`: `_` is a blank, the
/// `` ` ``/`a`..`~` positions map to box-drawing lines, scan lines, and symbols (including the `b`..`i`
/// control-picture glyphs). Anything outside that range passes through unchanged. The grid stores the
/// real Unicode char, which is what a full renderer draws.
fn dec_graphic(ch: char) -> char {
    match ch {
        '_' => '\u{00a0}', // NBSP (blank)
        '`' => '\u{25c6}', // ◆ diamond
        'a' => '\u{2592}', // ▒ checkerboard
        'b' => '\u{2409}', // ␉ HT
        'c' => '\u{240c}', // ␌ FF
        'd' => '\u{240d}', // ␍ CR
        'e' => '\u{240a}', // ␊ LF
        'f' => '\u{00b0}', // ° degree
        'g' => '\u{00b1}', // ± plus/minus
        'h' => '\u{2424}', // ␤ NL
        'i' => '\u{240b}', // ␋ VT
        'j' => '\u{2518}', // ┘ lower-right corner
        'k' => '\u{2510}', // ┐ upper-right corner
        'l' => '\u{250c}', // ┌ upper-left corner
        'm' => '\u{2514}', // └ lower-left corner
        'n' => '\u{253c}', // ┼ crossing
        'o' => '\u{23ba}', // ⎺ scan line 1
        'p' => '\u{23bb}', // ⎻ scan line 3
        'q' => '\u{2500}', // ─ horizontal (scan line 5)
        'r' => '\u{23bc}', // ⎼ scan line 7
        's' => '\u{23bd}', // ⎽ scan line 9
        't' => '\u{251c}', // ├ left tee
        'u' => '\u{2524}', // ┤ right tee
        'v' => '\u{2534}', // ┴ bottom tee
        'w' => '\u{252c}', // ┬ top tee
        'x' => '\u{2502}', // │ vertical
        'y' => '\u{2264}', // ≤ less-or-equal
        'z' => '\u{2265}', // ≥ greater-or-equal
        '{' => '\u{03c0}', // π pi
        '|' => '\u{2260}', // ≠ not-equal
        '}' => '\u{00a3}', // £ sterling
        '~' => '\u{00b7}', // · centered dot
        other => other,
    }
}
