//! The terminal cell grid: the screen state a VT parser writes into and a renderer draws from.
//!
//! A flat `rows × cols` array of [`Cell`]s plus a cursor. Colors and attributes are kept per-cell so
//! the renderer needs no parser state. Kept deliberately small and `Clone` so a render thread can take
//! an immutable snapshot while the parser keeps mutating the live grid.

use bitflags::bitflags;

/// A terminal color: the default fg/bg, one of the 256 indexed palette entries, or a direct RGB.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    /// The terminal's configured default (foreground or background depending on the slot).
    Default,
    /// An ANSI 256-color palette index (0..=15 are the classic named colors).
    Indexed(u8),
    /// A 24-bit true color.
    Rgb(u8, u8, u8),
}

bitflags! {
    /// Per-cell text rendition attributes (SGR).
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Attrs: u8 {
        const BOLD      = 1 << 0;
        const DIM       = 1 << 1;
        const ITALIC    = 1 << 2;
        const UNDERLINE = 1 << 3;
        const REVERSE   = 1 << 4;
        const HIDDEN    = 1 << 5;
        const STRIKE    = 1 << 6;
    }
}

/// One character cell.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    /// The glyph. A space (`' '`) is an empty cell.
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: Color::Default, bg: Color::Default, attrs: Attrs::empty() }
    }
}

impl Cell {
    /// A blank cell that keeps the given pen's colors/attrs (used when erasing so the cleared region
    /// adopts the current background, matching xterm's "erase with current SGR background" behavior).
    pub fn blank_with(fg: Color, bg: Color, attrs: Attrs) -> Cell {
        Cell { ch: ' ', fg, bg, attrs: attrs & Attrs::REVERSE } // keep only reverse for bg fill
    }
    /// Is this cell visually empty (a space with default colors/attrs)?
    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.bg == Color::Default && self.attrs.is_empty()
    }
}

/// The screen grid + cursor. `(0,0)` is the top-left cell.
#[derive(Clone, Debug)]
pub struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>, // row-major, rows*cols
    /// Cursor position (row, col), each in `0..rows` / `0..cols`.
    pub cursor_row: usize,
    pub cursor_col: usize,
    /// Whether the cursor should be drawn (DECTCEM).
    pub cursor_visible: bool,
}

impl Grid {
    /// A fresh blank grid of the given size (clamped to at least 1×1).
    pub fn new(cols: usize, rows: usize) -> Grid {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Grid {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }

    #[inline]
    fn idx(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    /// The cell at `(row, col)`; `None` if out of bounds.
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row < self.rows && col < self.cols {
            Some(&self.cells[self.idx(row, col)])
        } else {
            None
        }
    }

    /// Mutable access to `(row, col)`; `None` if out of bounds.
    pub fn cell_mut(&mut self, row: usize, col: usize) -> Option<&mut Cell> {
        if row < self.rows && col < self.cols {
            let i = self.idx(row, col);
            Some(&mut self.cells[i])
        } else {
            None
        }
    }

    /// The visible text of a row, trailing blanks trimmed — a convenience for tests/assertions.
    pub fn row_text(&self, row: usize) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = self.idx(row, 0);
        let s: String = self.cells[start..start + self.cols].iter().map(|c| c.ch).collect();
        s.trim_end().to_string()
    }

    /// Set `(row, col)` to `cell` if in bounds.
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if let Some(c) = self.cell_mut(row, col) {
            *c = cell;
        }
    }

    /// Fill `[col_start, col_end)` of `row` with a blank cell carrying the given pen bg/attrs.
    pub fn clear_row_range(&mut self, row: usize, col_start: usize, col_end: usize, blank: Cell) {
        if row >= self.rows {
            return;
        }
        let end = col_end.min(self.cols);
        for c in col_start..end {
            let i = self.idx(row, c);
            self.cells[i] = blank;
        }
    }

    /// Blank every cell (used by ED 2 and reset).
    pub fn clear_all(&mut self, blank: Cell) {
        for c in self.cells.iter_mut() {
            *c = blank;
        }
    }

    /// Scroll the whole grid up by one line: row 0 is lost, a blank line appears at the bottom.
    pub fn scroll_up(&mut self, blank: Cell) {
        // Move rows [1..rows) up to [0..rows-1).
        self.cells.copy_within(self.cols.., 0);
        let last = self.idx(self.rows - 1, 0);
        for c in &mut self.cells[last..last + self.cols] {
            *c = blank;
        }
    }

    /// Resize to `cols × rows`, preserving the top-left overlap. Cursor is clamped into range.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut next = vec![Cell::default(); cols * rows];
        for r in 0..rows.min(self.rows) {
            for c in 0..cols.min(self.cols) {
                next[r * cols + c] = self.cells[self.idx(r, c)];
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = next;
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_col = self.cursor_col.min(cols - 1);
    }
}
