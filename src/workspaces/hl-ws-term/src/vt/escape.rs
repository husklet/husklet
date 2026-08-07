use super::{Cell, Pen, State, Vt};

impl Vt {
    pub(super) fn enter_esc(&mut self) {
        self.state = State::Esc;
    }

    pub(super) fn esc(&mut self, b: u8) {
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
                    self.grid.scroll_region_down(self.scroll_top, self.scroll_bot, blank);
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
    pub(super) fn csi(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let d = (b - b'0') as u32;
                self.cur_param = Some(self.cur_param.unwrap_or(0).saturating_mul(10).saturating_add(d));
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
    pub(super) fn push_param(&mut self, colon: bool) {
        let v = self.cur_param.take().unwrap_or(0);
        if self.params.len() < 64 {
            self.params.push(v);
            self.param_colon.push(colon);
        }
    }

    pub(super) fn param(&self, i: usize, default: u32) -> u32 {
        self.params.get(i).copied().filter(|&v| v != 0).unwrap_or(default)
    }

    pub(super) fn dispatch_csi(&mut self, final_byte: u8) {
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
            b'L' if r >= self.scroll_top && r <= self.scroll_bot => {
                // IL: insert n blank lines at the cursor row within the scroll region.
                let n = self.param(0, 1);
                let blank = self.blank();
                for _ in 0..n {
                    self.grid.scroll_region_down(r, self.scroll_bot, blank);
                }
            }
            b'M' if r >= self.scroll_top && r <= self.scroll_bot => {
                // DL: delete n lines at the cursor row within the scroll region.
                let n = self.param(0, 1);
                let blank = self.blank();
                for _ in 0..n {
                    self.grid.scroll_region_up(r, self.scroll_bot, blank);
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
                    .map_or(rows - 1, |v| v as usize - 1);
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
                    self.grid.scroll_region_up(self.scroll_top, self.scroll_bot, blank);
                }
            }
            b'T' => {
                // SD: scroll the region down n lines.
                let n = self.param(0, 1);
                let blank = self.blank();
                for _ in 0..n {
                    self.grid.scroll_region_down(self.scroll_top, self.scroll_bot, blank);
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
}
