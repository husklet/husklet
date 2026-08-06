//! Regression tests for bugs found during the hl-term-core hardening pass.

use hl_ws_term::session::{Pane, PaneNode, Session, SessionTab, WorkingDirectory};
use hl_ws_term::{Attrs, Vt};

// --- session unesc / round-trip (was: Latin-1 byte-wise decode → mojibake + char-boundary panic) ---

#[test]
fn nonascii_title_and_cwd_roundtrip() {
    let s = Session {
        tabs: vec![SessionTab {
            title: "café ☕".to_string(),
            root: PaneNode::Leaf(Pane {
                cwd: Some("/home/joão/prjá".to_string()),
                history_file: None,
                slot: None,
            }),
        }],
    };
    let back = Session::parse(&s.serialize()).unwrap();
    assert_eq!(back.tabs[0].title, "café ☕", "title must survive round-trip");
    assert_eq!(back.tabs[0].root.leaves()[0].cwd.as_deref(), Some("/home/joão/prjá"));
}

#[test]
fn osc7_percent_encoded_utf8_path() {
    assert_eq!(
        WorkingDirectory::from_osc7("file://host/tmp/%C3%A9")
            .map(hl_ws_term::WorkingDirectory::into_string)
            .as_deref(),
        Some("/tmp/é")
    );
    assert_eq!(
        WorkingDirectory::from_osc7("file:///a/%E4%B8%96%E7%95%8C")
            .map(hl_ws_term::WorkingDirectory::into_string)
            .as_deref(),
        Some("/a/世界")
    );
}

#[test]
fn percent_before_literal_multibyte_does_not_panic() {
    // A stray/hand-edited '%' immediately before a literal multibyte char used to panic slicing mid-char.
    assert!(
        WorkingDirectory::from_osc7("file://h/%aé").is_some() || WorkingDirectory::from_osc7("file://h/%aé").is_none()
    );
    let _ = Session::parse("version 1\ntab %aé leaf /%zé - -\n").unwrap();
}

// --- VT: SGR colon-grouped underline style must not leak the style digit as a separate SGR code ---

#[test]
fn sgr_curly_underline_colon_does_not_set_italic() {
    let mut vt = Vt::new(4, 1);
    vt.advance_bytes(b"\x1b[4:3mX"); // undercurl
    let c = vt.grid().cell(0, 0).unwrap();
    assert!(c.attrs.contains(Attrs::UNDERLINE), "underline on");
    assert!(!c.attrs.contains(Attrs::ITALIC), "4:3 must not set italic");
}

#[test]
fn sgr_semicolon_four_three_is_underline_then_italic() {
    // The semicolon form keeps its old meaning: 4 = underline, 3 = italic.
    let mut vt = Vt::new(4, 1);
    vt.advance_bytes(b"\x1b[4;3mX");
    let c = vt.grid().cell(0, 0).unwrap();
    assert!(c.attrs.contains(Attrs::UNDERLINE));
    assert!(c.attrs.contains(Attrs::ITALIC));
}

#[test]
fn sgr_colon_underline_off() {
    let mut vt = Vt::new(4, 1);
    vt.advance_bytes(b"\x1b[4m\x1b[4:0mX"); // underline on, then style 0 = off
    assert!(!vt.grid().cell(0, 0).unwrap().attrs.contains(Attrs::UNDERLINE));
}

// --- VT: delete/insert-line on the LAST row of a scroll region must clear it, not no-op ---

#[test]
fn dl_at_region_bottom_clears_the_row() {
    let mut vt = Vt::new(6, 3);
    vt.advance_bytes(b"aaa\r\nbbb\r\nccc");
    vt.advance_bytes(b"\x1b[3;1H\x1b[1M"); // cursor to last row, DL 1
    assert_eq!(vt.grid().row_text(2), "", "DL at the region bottom must clear that row");
    assert_eq!(vt.grid().row_text(0), "aaa");
    assert_eq!(vt.grid().row_text(1), "bbb");
}

#[test]
fn il_at_region_bottom_clears_the_row() {
    let mut vt = Vt::new(6, 3);
    vt.advance_bytes(b"aaa\r\nbbb\r\nccc");
    vt.advance_bytes(b"\x1b[3;1H\x1b[1L"); // cursor to last row, IL 1
    assert_eq!(vt.grid().row_text(2), "", "IL at the region bottom must clear that row");
}

// --- VT: OSC 7 now surfaces the shell cwd (was silently dropped) ---

#[test]
fn osc7_sets_cwd_without_touching_title() {
    let mut vt = Vt::new(20, 3);
    vt.advance_bytes(b"\x1b]7;file://host/home/me/proj%20x\x07");
    assert_eq!(vt.cwd.as_deref(), Some("/home/me/proj x"));
    assert!(vt.title.is_empty(), "OSC 7 must not set the window title");
    // A later OSC 0 still sets the title and leaves cwd intact.
    vt.advance_bytes(b"\x1b]0;my title\x07");
    assert_eq!(vt.title, "my title");
    assert_eq!(vt.cwd.as_deref(), Some("/home/me/proj x"));
}
