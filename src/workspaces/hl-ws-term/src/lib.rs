//! hl-ws-term — the headless-testable terminal primitive of the hl terminal.
//!
//! Deliberately free of any windowing/GPU/network dependency (only `std` + `libc` + `bitflags`, all
//! already vendored) so the whole VT-grid → input → layout → CPU-render pipeline can be exercised with
//! plain `cargo test` on any host, with no display and no GPU and no crates.io access. The GPU shell
//! depends on this crate and adds only the window + wgpu draw, uploading this crate's same embedded
//! font atlas so what ships equals what the CPU renderer here verifies.
//!
//! This crate is the *terminal only*: it knows nothing about a Workspace/feature model. That model
//! (persistence, launch seam) lives in `hl-ws`, which depends on this crate for the [`pty::PtyBackend`]
//! seam its `Launcher` returns.

pub mod config;
pub mod font;
pub mod grid;
pub mod input;
pub mod launcher;
pub mod layout;
pub mod pty;
pub mod render;
pub mod session;
pub mod vt;

pub use config::{CursorShape, TermConfig};
pub use grid::{Attrs, Cell, Color, Grid};
pub use input::{encode_key, CursorKeys, Key, Mods, PasteMode};
pub use launcher::LocalShellLauncher;
pub use layout::{Dir, Layout, PaneId, Rect};
pub use pty::PtyBackend;
pub use render::{CpuRenderer, Image};
pub use session::{History, Pane, PaneNode, Session, SessionTab, SplitDir, WorkingDirectory};
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

    // ---- hardening: the audit backlog (alt screen, scroll regions, insert/delete, …) ----

    #[test]
    fn alt_screen_restores_primary_on_exit() {
        // The "vim leaves garbage" fix: enter alt (?1049h), draw, leave (?1049l) → the shell screen is
        // back exactly as it was.
        let mut vt = Vt::new(20, 4);
        vt.advance_bytes(b"shell prompt$ ");
        vt.advance_bytes(b"\x1b[?1049h"); // enter alt screen
        vt.advance_bytes(b"\x1b[2J\x1b[HVIM FULLSCREEN");
        assert_eq!(vt.grid().row_text(0), "VIM FULLSCREEN");
        vt.advance_bytes(b"\x1b[?1049l"); // leave alt screen
        assert_eq!(vt.grid().row_text(0), "shell prompt$");
    }

    #[test]
    fn scroll_region_pins_status_line() {
        // DECSTBM confines scrolling to [top,bottom]; a line below the region stays put.
        let mut vt = Vt::new(10, 4);
        vt.advance_bytes(b"\x1b[4;4Hstatus"); // row 4 (index 3) = a pinned status line
        vt.advance_bytes(b"\x1b[1;3r"); // scroll region = rows 1..3
        vt.advance_bytes(b"\x1b[1;1HA\r\nB\r\nC\r\nD"); // fill region; the last LF scrolls region only
                                                        // "status" was written at column 4 (index 3) so it reads with 3 leading spaces — and, crucially,
                                                        // it is still there: the scroll stayed inside rows 1..3 and left this pinned line untouched.
        assert_eq!(
            vt.grid().row_text(3),
            "   status",
            "status line outside the region must not scroll"
        );
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes(b"abcdef");
        vt.advance_bytes(b"\x1b[1G\x1b[2P"); // col 1, delete 2 chars -> "cdef"
        assert_eq!(vt.grid().row_text(0), "cdef");
        vt.advance_bytes(b"\x1b[1G\x1b[2@"); // col 1, insert 2 blanks -> "  cdef"
        assert_eq!(vt.grid().row_text(0), "  cdef");
        vt.advance_bytes(b"\x1b[1;3H\x1b[2X"); // col 3, erase 2 in place
        assert_eq!(vt.grid().cell(0, 2).unwrap().ch, ' ');
    }

    #[test]
    fn insert_and_delete_lines() {
        let mut vt = Vt::new(6, 4);
        vt.advance_bytes(b"one\r\ntwo\r\nthree");
        vt.advance_bytes(b"\x1b[1;1H\x1b[1L"); // insert a blank line at top
        assert_eq!(vt.grid().row_text(0), "");
        assert_eq!(vt.grid().row_text(1), "one");
        vt.advance_bytes(b"\x1b[1;1H\x1b[1M"); // delete it again
        assert_eq!(vt.grid().row_text(0), "one");
    }

    #[test]
    fn colon_sgr_truecolor_no_splatter() {
        // ISO-8613-6 colon form must set the color, NOT abort and dump the tail as literal text.
        let mut vt = Vt::new(20, 2);
        vt.advance_bytes(b"\x1b[38:2::10:20:30mX");
        assert_eq!(vt.grid().row_text(0), "X", "no literal '2::10:20:30m' splatter");
        assert_eq!(vt.grid().cell(0, 0).unwrap().fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn autowrap_off_overwrites_last_column() {
        let mut vt = Vt::new(4, 2);
        vt.advance_bytes(b"\x1b[?7l"); // DECAWM off
        vt.advance_bytes(b"abcdef"); // 4 cols; with autowrap off, cols 4.. overwrite the last cell
        assert_eq!(vt.grid().row_text(1), "", "must not have wrapped to row 1");
        assert_eq!(vt.grid().cell(0, 3).unwrap().ch, 'f'); // last written char sticks in the last column
    }

    #[test]
    fn dec_line_drawing_charset() {
        // ESC ( 0 selects DEC special graphics: q -> ─, x -> │. ESC ( B restores ASCII.
        let mut vt = Vt::new(10, 2);
        vt.advance_bytes(b"\x1b(0qx\x1b(Bq");
        assert_eq!(vt.grid().cell(0, 0).unwrap().ch, '\u{2500}'); // ─
        assert_eq!(vt.grid().cell(0, 1).unwrap().ch, '\u{2502}'); // │
        assert_eq!(vt.grid().cell(0, 2).unwrap().ch, 'q'); // back to ASCII
    }

    #[test]
    fn sco_cursor_save_restore() {
        let mut vt = Vt::new(20, 4);
        vt.advance_bytes(b"\x1b[2;5H\x1b[s"); // move + SCO save
        vt.advance_bytes(b"\x1b[4;10Hxxx"); // move elsewhere + write
        vt.advance_bytes(b"\x1b[uY"); // restore -> back at (2,5)
        assert_eq!(vt.grid().cell(1, 4).unwrap().ch, 'Y');
    }

    #[test]
    fn scroll_down_and_reverse_index() {
        let mut vt = Vt::new(6, 3);
        vt.advance_bytes(b"a\r\nb\r\nc"); // rows a/b/c
        vt.advance_bytes(b"\x1b[T"); // SD: scroll region down 1 -> blank, a, b
        assert_eq!(vt.grid().row_text(0), "");
        assert_eq!(vt.grid().row_text(1), "a");
    }
}
