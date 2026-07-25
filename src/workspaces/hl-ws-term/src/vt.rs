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

impl Pen {
    fn set_color(&mut self, foreground: bool, color: Color) {
        if foreground {
            self.fg = color;
        } else {
            self.bg = color;
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
}

mod escape;
mod input;
mod style;
