//! User terminal configuration (`~/.dd/term.conf`) — font, palette, cursor, default scrollback, and
//! keybindings — in a tiny dependency-free `key = value` format (the same style as `workspaces.conf`),
//! so the core stays free of serde/toml and this parser is fully headless-testable.
//!
//! Unknown keys are ignored (forward-compatible); malformed values fall back to the default. The GUI
//! (`dd-term`) loads this at startup, applies it to every VTE terminal, and live-reloads on file change.

use std::path::{Path, PathBuf};

/// Terminal cursor shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

impl CursorShape {
    pub fn as_str(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Beam => "beam",
            CursorShape::Underline => "underline",
        }
    }
    pub fn parse(s: &str) -> Option<CursorShape> {
        match s.trim().to_ascii_lowercase().as_str() {
            "block" => Some(CursorShape::Block),
            "beam" | "bar" | "ibeam" | "i-beam" => Some(CursorShape::Beam),
            "underline" | "under" | "underscore" => Some(CursorShape::Underline),
            _ => None,
        }
    }
}

/// The full, resolved terminal configuration. Every field always has a value (defaults applied at
/// load), so callers never deal with `Option` chasing.
#[derive(Clone, PartialEq, Debug)]
pub struct TermConfig {
    pub font_family: String,
    pub font_size: f64,
    /// Default scrollback for shells that don't override it (`None` = unlimited).
    pub scrollback: Option<u64>,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
    /// Foreground + background, then the 16 ANSI palette colors, as `#rrggbb` strings.
    pub foreground: String,
    pub background: String,
    pub palette: [String; 16],
    /// Optional keybinding overrides: action name → GTK accelerator (e.g. `new_tab = <Meta>t`). Only the
    /// actions the GUI knows are honored; unknown actions are ignored. Kept as raw strings so the core
    /// carries no GTK dependency.
    pub keybindings: Vec<(String, String)>,
}

/// The committed near-black defaults (mirror `term.rs`'s hard-coded look), so an absent/partial config
/// yields exactly today's appearance.
const DEFAULT_PALETTE: [&str; 16] = [
    "#2b2d33", "#ff5f56", "#5af78e", "#f3f99d", "#57c7ff", "#ff6ac1", "#9aedfe", "#c7ccd6", "#5c6370",
    "#ff6e67", "#5af78e", "#f4f99d", "#6dcbff", "#ff92d0", "#a5f0ff", "#ffffff",
];

impl Default for TermConfig {
    fn default() -> Self {
        TermConfig {
            font_family: "Menlo".to_string(),
            font_size: 12.0,
            scrollback: None,
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
            foreground: "#e7e9ee".to_string(),
            background: "#1a1d23".to_string(), // BG2 in term.rs
            palette: DEFAULT_PALETTE.map(|s| s.to_string()),
            keybindings: Vec::new(),
        }
    }
}

impl TermConfig {
    /// The Pango font description string VTE wants, e.g. `"Menlo 12"`.
    pub fn font_string(&self) -> String {
        // Trim a trailing `.0` so integer sizes read cleanly.
        if (self.font_size.fract()).abs() < f64::EPSILON {
            format!("{} {}", self.font_family, self.font_size as i64)
        } else {
            format!("{} {}", self.font_family, self.font_size)
        }
    }

    /// VTE scrollback-line count (same convention as `Workspace::scrollback_lines`: unlimited maps to a
    /// large file-backed cap).
    pub fn scrollback_lines(&self) -> i64 {
        match self.scrollback {
            None | Some(0) => 10_000_000,
            Some(n) => n as i64,
        }
    }

    /// Look up a keybinding override by action name.
    pub fn keybinding(&self, action: &str) -> Option<&str> {
        self.keybindings.iter().find(|(a, _)| a == action).map(|(_, v)| v.as_str())
    }

    /// Load config from `path`, applying defaults for anything missing/absent/malformed. A non-existent
    /// file yields `TermConfig::default()`.
    pub fn load(path: impl AsRef<Path>) -> TermConfig {
        let mut cfg = TermConfig::default();
        let Ok(text) = std::fs::read_to_string(path.as_ref()) else {
            return cfg;
        };
        cfg.apply_text(&text);
        cfg
    }

    /// Parse `key = value` lines into `self` (public for tests + live-reload).
    pub fn apply_text(&mut self, text: &str) {
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "font_family" | "font" if !v.is_empty() => {
                    // Allow `font = Menlo 13` (family + trailing size) as a convenience.
                    if k == "font" {
                        if let Some((fam, size)) = split_font(v) {
                            self.font_family = fam;
                            if let Some(s) = size {
                                self.font_size = s;
                            }
                            continue;
                        }
                    }
                    self.font_family = v.to_string();
                }
                "font_size" | "size" => {
                    if let Ok(n) = v.parse::<f64>() {
                        if n > 0.0 {
                            self.font_size = n;
                        }
                    }
                }
                "scrollback" => {
                    // `unlimited`/`0`/empty → unlimited; a number → cap.
                    self.scrollback = match v.to_ascii_lowercase().as_str() {
                        "" | "0" | "unlimited" | "infinite" | "inf" => None,
                        _ => v.parse::<u64>().ok().filter(|n| *n > 0),
                    };
                }
                "cursor_shape" | "cursor" => {
                    if let Some(cs) = CursorShape::parse(v) {
                        self.cursor_shape = cs;
                    }
                }
                "cursor_blink" | "blink" => {
                    self.cursor_blink = parse_bool(v).unwrap_or(self.cursor_blink);
                }
                "foreground" | "fg" if is_hex(v) => self.foreground = normalize_hex(v),
                "background" | "bg" if is_hex(v) => self.background = normalize_hex(v),
                _ => {
                    // color0..color15 = #rrggbb
                    if let Some(idx) = k.strip_prefix("color").and_then(|n| n.parse::<usize>().ok()) {
                        if idx < 16 && is_hex(v) {
                            self.palette[idx] = normalize_hex(v);
                        }
                    } else if let Some(action) = k.strip_prefix("key.") {
                        if !action.is_empty() && !v.is_empty() {
                            self.keybindings.retain(|(a, _)| a != action);
                            self.keybindings.push((action.to_string(), v.to_string()));
                        }
                    }
                }
            }
        }
    }

    /// A commented sample config (written on first run so users have something to edit).
    pub fn sample() -> String {
        let mut s = String::from("# dd terminal config — ~/.dd/term.conf\n# edit + save; open terminals live-reload.\n\n");
        s.push_str("font_family = Menlo\n");
        s.push_str("font_size = 12\n");
        s.push_str("# scrollback: a number of lines, or `unlimited`\n");
        s.push_str("scrollback = unlimited\n");
        s.push_str("cursor_shape = block   # block | beam | underline\n");
        s.push_str("cursor_blink = true\n\n");
        s.push_str("# colors (#rrggbb)\n");
        let d = TermConfig::default();
        s.push_str(&format!("foreground = {}\n", d.foreground));
        s.push_str(&format!("background = {}\n", d.background));
        for (i, c) in d.palette.iter().enumerate() {
            s.push_str(&format!("color{i} = {c}\n"));
        }
        s
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn is_hex(v: &str) -> bool {
    let v = v.trim();
    let body = v.strip_prefix('#').unwrap_or(v);
    (body.len() == 6 || body.len() == 3) && body.chars().all(|c| c.is_ascii_hexdigit())
}

fn normalize_hex(v: &str) -> String {
    let v = v.trim();
    if v.starts_with('#') {
        v.to_string()
    } else {
        format!("#{v}")
    }
}

/// Split a Pango-ish `"Family Name 13"` into `(family, Some(size))`, or `(whole, None)` if the last
/// token isn't a number.
fn split_font(v: &str) -> Option<(String, Option<f64>)> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if let Some((rest, last)) = v.rsplit_once(char::is_whitespace) {
        if let Ok(size) = last.parse::<f64>() {
            if size > 0.0 && !rest.trim().is_empty() {
                return Some((rest.trim().to_string(), Some(size)));
            }
        }
    }
    Some((v.to_string(), None))
}

/// The default config path, `<dd_root>/term.conf`.
pub fn config_path(dd_root: &Path) -> PathBuf {
    dd_root.join("term.conf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_shipped_look() {
        let c = TermConfig::default();
        assert_eq!(c.font_string(), "Menlo 12");
        assert_eq!(c.scrollback, None);
        assert_eq!(c.scrollback_lines(), 10_000_000);
        assert_eq!(c.cursor_shape, CursorShape::Block);
        assert!(c.cursor_blink);
        assert_eq!(c.palette[1], "#ff5f56");
        assert_eq!(c.foreground, "#e7e9ee");
    }

    #[test]
    fn parses_overrides() {
        let mut c = TermConfig::default();
        c.apply_text(
            "# comment\nfont_family = JetBrains Mono\nfont_size = 14\nscrollback = 5000\ncursor_shape = beam\ncursor_blink = off\nfg = #ffffff\ncolor1 = #ff0000\ncolor15 = fefefe\n",
        );
        assert_eq!(c.font_string(), "JetBrains Mono 14");
        assert_eq!(c.scrollback, Some(5000));
        assert_eq!(c.scrollback_lines(), 5000);
        assert_eq!(c.cursor_shape, CursorShape::Beam);
        assert!(!c.cursor_blink);
        assert_eq!(c.foreground, "#ffffff");
        assert_eq!(c.palette[1], "#ff0000");
        assert_eq!(c.palette[15], "#fefefe"); // normalized with leading #
    }

    #[test]
    fn font_combined_form() {
        let mut c = TermConfig::default();
        c.apply_text("font = SF Mono 13\n");
        assert_eq!(c.font_family, "SF Mono");
        assert_eq!(c.font_size, 13.0);
        assert_eq!(c.font_string(), "SF Mono 13");
    }

    #[test]
    fn scrollback_unlimited_word() {
        let mut c = TermConfig::default();
        c.scrollback = Some(10);
        c.apply_text("scrollback = unlimited\n");
        assert_eq!(c.scrollback, None);
    }

    #[test]
    fn keybindings_override() {
        let mut c = TermConfig::default();
        c.apply_text("key.new_tab = <Meta>n\nkey.split = <Meta><Shift>d\n");
        assert_eq!(c.keybinding("new_tab"), Some("<Meta>n"));
        assert_eq!(c.keybinding("split"), Some("<Meta><Shift>d"));
        assert_eq!(c.keybinding("nonexistent"), None);
    }

    #[test]
    fn absent_file_is_default() {
        let c = TermConfig::load("/nonexistent/dd/term.conf");
        assert_eq!(c, TermConfig::default());
    }

    #[test]
    fn malformed_values_keep_defaults() {
        let mut c = TermConfig::default();
        c.apply_text("font_size = notanumber\nscrollback = abc\ncolor99 = #fff\ncolor1 = nothex\n");
        assert_eq!(c.font_size, 12.0);
        assert_eq!(c.scrollback, None); // "abc" -> None (unlimited) is acceptable fallback
        assert_eq!(c.palette[1], "#ff5f56"); // unchanged
    }

    #[test]
    fn sample_roundtrips_to_default() {
        let sample = TermConfig::sample();
        let mut c = TermConfig::default();
        // mutate away from default first, then re-apply the sample → back to default values.
        c.font_size = 99.0;
        c.apply_text(&sample);
        assert_eq!(c, TermConfig::default());
    }
}
