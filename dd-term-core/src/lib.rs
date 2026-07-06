//! dd-term-core — the headless-testable core of the dd terminal.
//!
//! Deliberately free of any windowing/GPU/network dependency (only `std` + `libc` + `bitflags`, all
//! already vendored) so the whole VT-grid → input → layout → CPU-render pipeline can be exercised with
//! plain `cargo test` on any host, with no display and no GPU and no crates.io access. The GPU shell
//! (`dd-term`) depends on this crate and adds only the window + wgpu draw, uploading this crate's same
//! embedded font atlas so what ships equals what the CPU renderer here verifies.

pub mod grid;
pub mod pty;
pub mod vt;

pub use grid::{Attrs, Cell, Color, Grid};
pub use pty::PtyBackend;
pub use vt::Vt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_and_newlines() {
        let mut vt = Vt::new(20, 5);
        vt.advance_bytes(b"hello\r\nworld");
        assert_eq!(vt.grid().row_text(0), "hello");
        assert_eq!(vt.grid().row_text(1), "world");
        assert_eq!((vt.grid().cursor_row, vt.grid().cursor_col), (1, 5));
    }

    #[test]
    fn autowrap_at_last_column() {
        let mut vt = Vt::new(4, 3);
        vt.advance_bytes(b"abcdef"); // 4 cols: "abcd" then wrap "ef"
        assert_eq!(vt.grid().row_text(0), "abcd");
        assert_eq!(vt.grid().row_text(1), "ef");
    }

    #[test]
    fn lf_at_bottom_scrolls() {
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes(b"one\r\ntwo\r\nthree");
        // "one" scrolled off the top; "two" and "three" remain.
        assert_eq!(vt.grid().row_text(0), "two");
        assert_eq!(vt.grid().row_text(1), "three");
    }

    #[test]
    fn cursor_position_cup() {
        let mut vt = Vt::new(20, 10);
        vt.advance_bytes(b"\x1b[3;5HX"); // row 3, col 5 (1-based) -> (2,4)
        assert_eq!(vt.grid().cell(2, 4).unwrap().ch, 'X');
    }

    #[test]
    fn sgr_colors_and_bold() {
        let mut vt = Vt::new(20, 3);
        vt.advance_bytes(b"\x1b[1;31mR\x1b[0mn");
        let r = vt.grid().cell(0, 0).unwrap();
        assert_eq!(r.ch, 'R');
        assert_eq!(r.fg, Color::Indexed(1)); // red
        assert!(r.attrs.contains(Attrs::BOLD));
        let n = vt.grid().cell(0, 1).unwrap();
        assert_eq!(n.fg, Color::Default);
        assert!(!n.attrs.contains(Attrs::BOLD));
    }

    #[test]
    fn truecolor_sgr() {
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes(b"\x1b[38;2;10;20;30mT");
        assert_eq!(vt.grid().cell(0, 0).unwrap().fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn erase_line_and_display() {
        let mut vt = Vt::new(10, 3);
        vt.advance_bytes(b"ABCDE\x1b[3G\x1b[0K"); // write, cursor to col3, erase to EOL
        assert_eq!(vt.grid().row_text(0), "AB");
        vt.advance_bytes(b"\x1b[2J"); // clear screen
        assert_eq!(vt.grid().row_text(0), "");
    }

    #[test]
    fn utf8_multibyte() {
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes("héllo→".as_bytes());
        assert_eq!(vt.grid().row_text(0), "héllo→");
    }

    #[test]
    fn osc_title() {
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes(b"\x1b]0;my title\x07");
        assert_eq!(vt.title, "my title");
        // The BEL that terminates an OSC is a string terminator, NOT an audible bell.
        assert!(!vt.bell);
    }

    #[test]
    fn ground_bel_rings() {
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes(b"a\x07b");
        assert!(vt.bell);
        assert_eq!(vt.grid().row_text(0), "ab"); // BEL doesn't advance the cursor
    }

    #[test]
    fn cursor_hide_show() {
        let mut vt = Vt::new(5, 2);
        vt.advance_bytes(b"\x1b[?25l");
        assert!(!vt.grid().cursor_visible);
        vt.advance_bytes(b"\x1b[?25h");
        assert!(vt.grid().cursor_visible);
    }

    #[test]
    fn resize_preserves_topleft() {
        let mut vt = Vt::new(10, 4);
        vt.advance_bytes(b"hello");
        vt.resize(3, 4);
        assert_eq!(vt.grid().row_text(0), "hel");
        assert_eq!(vt.grid().cols(), 3);
    }
}
