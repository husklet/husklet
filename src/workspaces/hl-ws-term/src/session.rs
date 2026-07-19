//! Per-workspace terminal **session layout** — the tmux-like structural layer over a workspace: which
//! tabs exist, how each tab is split into panes, and each pane's title, cwd, and saved scrollback
//! history. Persisted next to the workspace's storage (`<storage_dir>/session/`) so reopening a
//! workspace restores its whole tab/split layout (and the on-screen history above each resumed prompt),
//! not just a single fresh shell.
//!
//! Dependency-free + fully headless-testable: the tree serializes to a compact prefix-notation text
//! format (no serde/toml), and the history round-trip (dump VTE text → replay bytes) is pure.

use std::path::{Path, PathBuf};

/// Split orientation of a [`PaneNode::Split`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// Panes side by side (a horizontal GtkPaned).
    Horizontal,
    /// Panes stacked (a vertical GtkPaned).
    Vertical,
}

impl SplitDir {
    fn token(self) -> &'static str {
        match self {
            SplitDir::Horizontal => "hsplit",
            SplitDir::Vertical => "vsplit",
        }
    }
}

/// A node in a tab's pane tree: either a terminal leaf or a binary split.
#[derive(Clone, PartialEq, Debug)]
pub enum PaneNode {
    Leaf(Pane),
    Split {
        dir: SplitDir,
        ratio: f64,
        a: Box<PaneNode>,
        b: Box<PaneNode>,
    },
}

/// A single terminal pane's restorable state.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Pane {
    /// The pane's last cwd (a plain path, decoded from OSC 7's `file://` URI), if known.
    pub cwd: Option<String>,
    /// Relative filename (under the session dir) holding this pane's saved scrollback text, if any.
    pub history_file: Option<String>,
    /// Stable per-pane layout identity persisted across close and reopen.
    pub slot: Option<String>,
}

impl PaneNode {
    pub fn leaf() -> PaneNode {
        PaneNode::Leaf(Pane::default())
    }
    /// Iterate the leaves left-to-right (pre-order), for assigning/reading history files.
    pub fn leaves(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }
    fn collect<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneNode::Leaf(p) => out.push(p),
            PaneNode::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Leaf(pane) => {
                out.push_str("leaf ");
                out.push_str(&Layout::escape(pane.cwd.as_deref().unwrap_or("-")));
                out.push(' ');
                out.push_str(&Layout::escape(pane.history_file.as_deref().unwrap_or("-")));
                out.push(' ');
                out.push_str(&Layout::escape(pane.slot.as_deref().unwrap_or("-")));
            }
            Self::Split { dir, ratio, a, b } => {
                out.push_str(dir.token());
                out.push(' ');
                out.push_str(&format!("{:.4}", ratio.clamp(0.05, 0.95)));
                out.push(' ');
                a.write(out);
                out.push(' ');
                b.write(out);
            }
        }
    }
}

/// One tab: a title + its pane tree.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionTab {
    pub title: String,
    pub root: PaneNode,
}

/// A workspace's whole terminal session (its ordered tabs). Persisted to `session/layout.conf`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
}

impl Session {
    /// The session directory for a workspace storage dir.
    pub fn dir(storage_dir: &Path) -> PathBuf {
        storage_dir.join("session")
    }
    fn layout_path(storage_dir: &Path) -> PathBuf {
        Self::dir(storage_dir).join("layout.conf")
    }

    /// Serialize to the prefix-notation text format.
    pub fn serialize(&self) -> String {
        let mut out = String::from("# hl session layout\nversion 1\n");
        for tab in &self.tabs {
            out.push_str("tab ");
            out.push_str(&Layout::escape(&tab.title));
            out.push(' ');
            tab.root.write(&mut out);
            out.push('\n');
        }
        out
    }

    /// Parse the text format (best-effort: malformed tabs are skipped).
    pub fn parse(text: &str) -> Session {
        let mut tabs = Vec::new();
        // Tokenize the whole file by whitespace; prefix notation is self-delimiting.
        let toks: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .flat_map(|l| l.split_whitespace())
            .collect();
        let mut layout = Layout::new(&toks);
        while let Some(token) = layout.peek() {
            match token {
                "tab" => {
                    layout.next();
                    let Some(title) = layout.next().map(Layout::unescape) else {
                        break;
                    };
                    if let Some(root) = layout.node() {
                        tabs.push(SessionTab { title, root });
                    }
                }
                _ => {
                    layout.next();
                }
            }
        }
        Session { tabs }
    }

    /// Load a workspace's session from disk (absent = empty session).
    pub fn load(storage_dir: &Path) -> Session {
        match std::fs::read_to_string(Self::layout_path(storage_dir)) {
            Ok(text) => Self::parse(&text),
            Err(_) => Session::default(),
        }
    }

    /// Persist the layout to disk (creating the session dir).
    pub fn save(&self, storage_dir: &Path) -> std::io::Result<()> {
        let dir = Self::dir(storage_dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(Self::layout_path(storage_dir), self.serialize())
    }

    /// Remove the whole persisted session (layout + all history files).
    pub fn clear(storage_dir: &Path) {
        let _ = std::fs::remove_dir_all(Self::dir(storage_dir));
    }

    /// Absolute path of a pane's history file within the session dir.
    pub fn history_path(storage_dir: &Path, file: &str) -> PathBuf {
        Self::dir(storage_dir).join(file)
    }
}

struct Layout<'a> {
    tokens: &'a [&'a str],
    index: usize,
}

impl<'a> Layout<'a> {
    fn new(tokens: &'a [&'a str]) -> Self {
        Self { tokens, index: 0 }
    }

    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.index).copied()
    }

    fn next(&mut self) -> Option<&'a str> {
        let token = self.peek()?;
        self.index += 1;
        Some(token)
    }

    fn node(&mut self) -> Option<PaneNode> {
        match self.next()? {
            "leaf" => {
                let cwd = self.next().map(Self::value).unwrap_or(None);
                let history_file = self.next().map(Self::value).unwrap_or(None);
                // Optional third field: the pane's stable layout slot. Old session files have only
                // cwd + history_file, where the NEXT token is a structural keyword (`leaf`/`hsplit`/`vsplit`/
                // `tab`) or EOF — so only consume a third token when it is NOT one of those, i.e. a real slot.
                let slot = match self.peek() {
                    Some(t) if !matches!(t, "leaf" | "hsplit" | "vsplit" | "tab") => {
                        self.next();
                        Self::value(t)
                    }
                    _ => None,
                };
                Some(PaneNode::Leaf(Pane {
                    cwd,
                    history_file,
                    slot,
                }))
            }
            direction @ ("hsplit" | "vsplit") => {
                let dir = if direction == "hsplit" {
                    SplitDir::Horizontal
                } else {
                    SplitDir::Vertical
                };
                let ratio = self
                    .next()
                    .and_then(|value| value.parse::<f64>().ok())
                    .unwrap_or(0.5);
                let a = self.node()?;
                let b = self.node()?;
                Some(PaneNode::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                })
            }
            _ => None,
        }
    }

    fn value(token: &str) -> Option<String> {
        let value = Self::unescape(token);
        (!value.is_empty() && value != "-").then_some(value)
    }

    /// Percent-escape whitespace and `%` so each value remains one layout token.
    fn escape(s: &str) -> String {
        if s.is_empty() {
            return "-".to_string();
        }
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '%' => out.push_str("%25"),
                ' ' => out.push_str("%20"),
                '\t' => out.push_str("%09"),
                '\n' => out.push_str("%0A"),
                '\r' => out.push_str("%0D"),
                _ => out.push(c),
            }
        }
        out
    }

    fn unescape(s: &str) -> String {
        // Decode into a BYTE buffer first, then interpret the whole thing as UTF-8. Percent-escapes encode
        // raw bytes (a multibyte char is `%C3%A9`, etc.), so decoding byte-by-byte and only then running
        // UTF-8 reassembly is what makes non-ASCII titles/cwds round-trip. (The previous version pushed each
        // decoded byte as a `char`, mangling every multibyte codepoint — and could even panic slicing a
        // literal multibyte char that happened to follow a `%`.)
        let bytes = s.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 3 <= bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit()
            {
                let hi = (bytes[i + 1] as char).to_digit(16).unwrap() as u8;
                let lo = (bytes[i + 2] as char).to_digit(16).unwrap() as u8;
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        // Lossy so a malformed/hand-edited escape can never make the whole parser fail.
        String::from_utf8_lossy(&out).into_owned()
    }
}

// ---- history dump / replay ---------------------------------------------------------------------

/// Turn a workspace's saved scrollback+screen text (as extracted from VTE via `get_text_*`) into bytes
/// safe to `feed()` back into a fresh VTE on restore: trailing blank lines trimmed and `\n` normalized to
/// `\r\n` (VTE is a raw terminal — a bare LF would stair-step). Returns an empty vec for empty/
/// whitespace-only history (nothing to replay).
pub struct History<'a>(&'a str);

impl<'a> History<'a> {
    pub fn new(text: &'a str) -> Self {
        Self(text)
    }

    pub fn replay(&self) -> Vec<u8> {
        let trimmed = self.0.trim_end_matches(['\n', '\r', ' ', '\t']);
        if trimmed.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(trimmed.len() + 8);
        for line in trimmed.split('\n') {
            out.extend_from_slice(line.trim_end_matches('\r').as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    /// Clamp saved history to at most `max_lines` (keep the most recent), so a huge scrollback dump doesn't
    /// balloon the on-disk session or the replay.
    pub fn clamp(&self, max_lines: usize) -> String {
        let lines: Vec<&str> = self.0.trim_end_matches('\n').split('\n').collect();
        if lines.len() <= max_lines {
            return self.0.to_string();
        }
        lines[lines.len() - max_lines..].join("\n")
    }
}

/// Decode an OSC 7 `file://host/path` URI to a plain filesystem path (percent-decoded). Returns `None`
/// for a non-`file` URI or an empty path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkingDirectory(String);

impl WorkingDirectory {
    pub fn from_osc7(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("file://")?;
        // Strip the authority (hostname) up to the first '/'.
        let path = match rest.find('/') {
            Some(i) => &rest[i..],
            None => return None,
        };
        let decoded = Self::decode(path);
        if decoded.is_empty() {
            None
        } else {
            Some(Self(decoded))
        }
    }

    pub fn into_string(self) -> String {
        self.0
    }

    fn decode(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%'
                && bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)
            {
                let high = (bytes[index + 1] as char).to_digit(16).unwrap() as u8;
                let low = (bytes[index + 2] as char).to_digit(16).unwrap() as u8;
                decoded.push(high << 4 | low);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8_lossy(&decoded).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> Session {
        Session {
            tabs: vec![
                SessionTab {
                    title: "shell 1".to_string(),
                    root: PaneNode::Leaf(Pane {
                        cwd: Some("/root/my project".to_string()),
                        history_file: Some("hist-0.txt".to_string()),
                        slot: Some("0".to_string()),
                    }),
                },
                SessionTab {
                    title: "build".to_string(),
                    root: PaneNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.5,
                        a: Box::new(PaneNode::Leaf(Pane {
                            cwd: None,
                            history_file: None,
                            slot: Some("1".to_string()),
                        })),
                        b: Box::new(PaneNode::Split {
                            dir: SplitDir::Vertical,
                            ratio: 0.3,
                            a: Box::new(PaneNode::Leaf(Pane {
                                cwd: Some("/tmp".to_string()),
                                history_file: None,
                                slot: Some("2".to_string()),
                            })),
                            b: Box::new(PaneNode::leaf()),
                        }),
                    },
                },
            ],
        }
    }

    #[test]
    fn layout_roundtrips() {
        let s = sample_session();
        let text = s.serialize();
        let back = Session::parse(&text);
        assert_eq!(back.tabs.len(), 2);
        assert_eq!(back.tabs[0].title, "shell 1");
        // ratio is formatted to 4 decimals; compare the structure with tolerance.
        assert_eq!(back.tabs[0].root, s.tabs[0].root);
        assert_eq!(back.tabs[1].root, s.tabs[1].root);
        // Each pane's layout slot round-trips.
        assert_eq!(back.tabs[0].root.leaves()[0].slot.as_deref(), Some("0"));
        assert_eq!(back.tabs[1].root.leaves()[0].slot.as_deref(), Some("1"));
        assert_eq!(back.tabs[1].root.leaves()[1].slot.as_deref(), Some("2"));
    }

    #[test]
    fn old_layout_without_slots_still_parses() {
        // Pre-slot session files wrote only `leaf <cwd> <history>` (two fields). They must still load,
        // with every pane's slot defaulting to None (a fresh slot is allocated on reopen).
        let text = "version 1\ntab shell%201 leaf /root hist-0.txt\ntab build hsplit 0.5 leaf /a - leaf /b -\n";
        let s = Session::parse(text);
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].root.leaves()[0].cwd.as_deref(), Some("/root"));
        assert_eq!(
            s.tabs[0].root.leaves()[0].history_file.as_deref(),
            Some("hist-0.txt")
        );
        assert_eq!(s.tabs[0].root.leaves()[0].slot, None);
        let build = s.tabs[1].root.leaves();
        assert_eq!(build.len(), 2);
        assert_eq!(build[0].cwd.as_deref(), Some("/a"));
        assert_eq!(build[1].cwd.as_deref(), Some("/b"));
        assert!(build.iter().all(|p| p.slot.is_none()));
    }

    #[test]
    fn escaping_survives_spaces_and_specials() {
        let s = Session {
            tabs: vec![SessionTab {
                title: "a b%c".to_string(),
                root: PaneNode::Leaf(Pane {
                    cwd: Some("/p a/th".to_string()),
                    history_file: None,
                    slot: None,
                }),
            }],
        };
        let back = Session::parse(&s.serialize());
        assert_eq!(back.tabs[0].title, "a b%c");
        assert_eq!(
            back.tabs[0].root.leaves()[0].cwd.as_deref(),
            Some("/p a/th")
        );
    }

    #[test]
    fn empty_and_absent_are_empty() {
        assert_eq!(Session::parse("").tabs.len(), 0);
        assert_eq!(
            Session::parse("# just a comment\nversion 1\n").tabs.len(),
            0
        );
    }

    #[test]
    fn malformed_layout_skips_incomplete_trees_without_panicking() {
        let session = Session::parse(
            "version 1\ntab valid leaf /ok hist 7\ntab broken hsplit nope leaf /a -\n",
        );
        assert_eq!(session.tabs.len(), 1);
        assert_eq!(session.tabs[0].title, "valid");
        assert_eq!(session.tabs[0].root.leaves()[0].cwd.as_deref(), Some("/ok"));
    }

    #[test]
    fn malformed_percent_escape_is_preserved() {
        let session = Session::parse("tab bad%zz leaf /tmp - -");
        assert_eq!(session.tabs[0].title, "bad%zz");
    }

    #[test]
    fn leaves_are_left_to_right() {
        let s = sample_session();
        let leaves = s.tabs[1].root.leaves();
        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[1].cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn save_load_via_disk() {
        let dir = std::env::temp_dir().join(format!("hl-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = sample_session();
        s.save(&dir).unwrap();
        let back = Session::load(&dir);
        assert_eq!(back, s);
        Session::clear(&dir);
        assert_eq!(Session::load(&dir), Session::default());
    }

    #[test]
    fn replay_bytes_normalizes() {
        let bytes = History::new("line one\nline two\n\n\n").replay();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("line one\r\nline two\r\n"));
        assert!(!s.contains("\n\n\n")); // trailing blanks trimmed
        assert!(History::new("   \n\n").replay().is_empty());
    }

    #[test]
    fn clamp_keeps_most_recent() {
        let text = (0..100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let clamped = History::new(&text).clamp(10);
        let lines: Vec<&str> = clamped.split('\n').collect();
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "90");
        assert_eq!(lines[9], "99");
    }

    #[test]
    fn cwd_uri_decoding() {
        assert_eq!(
            WorkingDirectory::from_osc7("file://host/root/my%20dir")
                .map(|path| path.into_string())
                .as_deref(),
            Some("/root/my dir")
        );
        assert_eq!(
            WorkingDirectory::from_osc7("file:///tmp/x")
                .map(|path| path.into_string())
                .as_deref(),
            Some("/tmp/x")
        );
        assert_eq!(WorkingDirectory::from_osc7("http://x/y"), None);
    }
}
