//! Per-workspace terminal **session layout** — the tmux-like structural layer over a workspace: which
//! tabs exist, how each tab is split into panes, and each pane's title, cwd, and saved scrollback
//! history. Persisted next to the workspace's storage (`<storage_dir>/session/`) so reopening a
//! workspace restores its whole tab/split layout (and the on-screen history above each resumed prompt),
//! not just a single fresh shell.
//!
//! Dependency-free + fully headless-testable: the tree serializes to a compact prefix-notation text
//! format (no serde/toml), and the history round-trip (dump VTE text → replay bytes) is pure.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{collections::HashSet, io};

/// Split orientation of a [`PaneNode::Split`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// Panes side by side (a horizontal `GtkPaned`).
    Horizontal,
    /// Panes stacked (a vertical `GtkPaned`).
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

/// A node in a tab's pane tree: a terminal leaf, a leaf an extension draws
/// into, or a binary split.
#[derive(Clone, PartialEq, Debug)]
pub enum PaneNode {
    Leaf(Pane),
    /// A leaf holding an extension's interface instead of a shell.
    Surface(SurfacePane),
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

/// A pane an extension renders its interface into.
///
/// Only the identity is persisted, never the drawing: an interface is a stream
/// of reconciliation frames from a running extension, and a picture of one is
/// not something this layer can hold or replay. What is restored is therefore
/// the pane's place in the layout and the name of what belongs in it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SurfacePane {
    /// The extension whose interface belongs in this pane.
    pub extension: String,
    /// Named provider drawn here; absent for layouts written before providers.
    pub provider: Option<String>,
    /// Stable per-pane layout identity persisted across close and reopen.
    pub slot: Option<String>,
}

impl PaneNode {
    #[must_use]
    pub fn leaf() -> PaneNode {
        PaneNode::Leaf(Pane::default())
    }
    /// Iterate the leaves left-to-right (pre-order), for assigning/reading history files.
    #[must_use]
    pub fn leaves(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }
    fn collect<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneNode::Leaf(p) => out.push(p),
            // A surface holds no shell and therefore no scrollback file.
            PaneNode::Surface(_) => {}
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
            Self::Surface(pane) => {
                out.push_str("surface ");
                let identity = pane.provider.as_ref().map_or_else(
                    || pane.extension.clone(),
                    |provider| format!("{}@{provider}", pane.extension),
                );
                out.push_str(&Layout::escape(&identity));
                out.push(' ');
                out.push_str(&Layout::escape(pane.slot.as_deref().unwrap_or("-")));
            }
            Self::Split { dir, ratio, a, b } => {
                out.push_str(dir.token());
                out.push(' ');
                let _ = write!(out, "{:.4}", ratio.clamp(0.05, 0.95));
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
    #[must_use]
    pub fn dir(storage_dir: &Path) -> PathBuf {
        storage_dir.join("session")
    }
    fn layout_path(storage_dir: &Path) -> PathBuf {
        Self::dir(storage_dir).join("layout.conf")
    }

    /// Serialize to the prefix-notation text format.
    #[must_use]
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

    /// Parse a complete session document.
    ///
    /// # Errors
    ///
    /// Returns an invalid-data error when the version, structure, or values are malformed.
    pub fn parse(text: &str) -> io::Result<Session> {
        // Tokenize the whole file by whitespace; prefix notation is self-delimiting.
        let toks: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .flat_map(|l| l.split_whitespace())
            .collect();
        let mut layout = Layout::new(&toks);
        if layout.next() != Some("version") || layout.next() != Some("1") {
            return Err(Layout::invalid("missing supported `version 1` header"));
        }
        let mut tabs = Vec::new();
        while layout.peek().is_some() {
            if layout.next() != Some("tab") {
                return Err(Layout::invalid("expected `tab`"));
            }
            let title = layout
                .next()
                .map(Layout::unescape)
                .ok_or_else(|| Layout::invalid("tab is missing its title"))?;
            let root = layout.node()?;
            tabs.push(SessionTab { title, root });
        }
        Ok(Session { tabs })
    }

    /// Opens a workspace session, distinguishing an absent session from unreadable or malformed state.
    ///
    /// # Errors
    ///
    /// Returns filesystem read failures and invalid layout documents.
    pub fn open(storage_dir: &Path) -> io::Result<Session> {
        match std::fs::read_to_string(Self::layout_path(storage_dir)) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Session::default()),
            Err(error) => Err(error),
        }
    }

    /// Atomically persists the layout after its referenced history files have been written.
    ///
    /// # Errors
    ///
    /// Returns directory creation, write, synchronization, rename, or obsolete-history cleanup
    /// failures. A failed layout replacement leaves the previous complete layout readable.
    pub fn save(&self, storage_dir: &Path) -> io::Result<()> {
        let dir = Self::dir(storage_dir);
        std::fs::create_dir_all(&dir)?;
        hl_fs::File::from(Self::layout_path(storage_dir)).replace(self.serialize())?;
        self.prune(storage_dir)
    }

    /// Remove the whole persisted session (layout + all history files). An absent session is clear.
    ///
    /// # Errors
    ///
    /// Returns filesystem removal failures other than an absent directory.
    pub fn clear(storage_dir: &Path) -> io::Result<()> {
        match std::fs::remove_dir_all(Self::dir(storage_dir)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Resolve a pane history filename within the session directory.
    ///
    /// # Errors
    ///
    /// Rejects absolute paths, parent traversal, nested paths, and names outside the history-file
    /// namespace. Layout files are persistent input and must not become arbitrary filesystem reads.
    pub fn history_path(storage_dir: &Path, file: &str) -> io::Result<PathBuf> {
        let mut components = Path::new(file).components();
        let valid_component =
            matches!(components.next(), Some(std::path::Component::Normal(_))) && components.next().is_none();
        if !valid_component
            || !file.starts_with("hist-")
            || !std::path::Path::new(file)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
        {
            return Err(Layout::invalid("invalid pane history filename"));
        }
        Ok(Self::dir(storage_dir).join(file))
    }

    fn prune(&self, storage_dir: &Path) -> io::Result<()> {
        let retained: HashSet<&str> = self
            .tabs
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .filter_map(|pane| pane.history_file.as_deref())
            .collect();
        for entry in std::fs::read_dir(Self::dir(storage_dir))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with("hist-") && !retained.contains(name) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
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

    fn node(&mut self) -> io::Result<PaneNode> {
        match self
            .next()
            .ok_or_else(|| Layout::invalid("pane tree ended unexpectedly"))?
        {
            "leaf" => {
                let cwd = Self::value(
                    self.next()
                        .ok_or_else(|| Layout::invalid("leaf is missing its directory"))?,
                );
                let history_file = Self::value(
                    self.next()
                        .ok_or_else(|| Layout::invalid("leaf is missing its history"))?,
                );
                if let Some(file) = history_file.as_deref() {
                    Session::history_path(Path::new(""), file)?;
                }
                let slot = Self::value(self.next().ok_or_else(|| Layout::invalid("leaf is missing its slot"))?);
                Ok(PaneNode::Leaf(Pane {
                    cwd,
                    history_file,
                    slot,
                }))
            }
            "surface" => {
                let identity = Self::value(
                    self.next()
                        .ok_or_else(|| Layout::invalid("surface is missing its extension"))?,
                )
                .ok_or_else(|| Layout::invalid("surface is missing its extension"))?;
                let (extension, provider) = identity.split_once('@').map_or_else(
                    || (identity.clone(), None),
                    |(extension, provider)| (extension.to_owned(), Some(provider.to_owned())),
                );
                let slot = Self::value(
                    self.next()
                        .ok_or_else(|| Layout::invalid("surface is missing its slot"))?,
                );
                Ok(PaneNode::Surface(SurfacePane {
                    extension,
                    provider,
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
                    .ok_or_else(|| Layout::invalid("split is missing its ratio"))?
                    .parse::<f64>()
                    .map_err(|_| Layout::invalid("split ratio is not a number"))?;
                if !ratio.is_finite() {
                    return Err(Layout::invalid("split ratio is not finite"));
                }
                let a = self.node()?;
                let b = self.node()?;
                Ok(PaneNode::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                })
            }
            _ => Err(Layout::invalid("unknown pane type")),
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

    fn invalid(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }
}

// ---- history dump / replay ---------------------------------------------------------------------

/// Turn a workspace's saved scrollback+screen text (as extracted from VTE via `get_text_*`) into bytes
/// safe to `feed()` back into a fresh VTE on restore: trailing blank lines trimmed and `\n` normalized to
/// `\r\n` (VTE is a raw terminal — a bare LF would stair-step). Returns an empty vec for empty/
/// whitespace-only history (nothing to replay).
pub struct History<'a>(&'a str);

impl<'a> History<'a> {
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self(text)
    }

    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn from_osc7(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("file://")?;
        // Strip the authority (hostname) up to the first '/'.
        let path = match rest.find('/') {
            Some(i) => &rest[i..],
            None => return None,
        };
        let decoded = Self::decode(path);
        if decoded.is_empty() || decoded.bytes().any(|byte| byte.is_ascii_control()) {
            None
        } else {
            Some(Self(decoded))
        }
    }

    #[must_use]
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
mod working_directory_tests {
    use super::WorkingDirectory;

    #[test]
    fn osc7_preserves_percent_encoded_unicode_paths() {
        let cwd = WorkingDirectory::from_osc7("file://guest/work/%E4%B8%AD").unwrap();
        assert_eq!(cwd.into_string(), "/work/中");
    }

    #[test]
    fn osc7_rejects_literal_and_percent_encoded_controls() {
        for uri in [
            "file://guest/work/%00hidden",
            "file://guest/work/%0Ahidden",
            "file://guest/work/%09hidden",
            "file://guest/work/\nnewline",
            "file://guest/work/\x7fdelete",
        ] {
            assert!(WorkingDirectory::from_osc7(uri).is_none(), "accepted {uri:?}");
        }
    }
}

#[cfg(test)]
#[path = "session/test.rs"]
mod test;
