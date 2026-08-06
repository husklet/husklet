//! User terminal configuration (`~/.hl/term.conf`) — font, palette, cursor, default scrollback, and
//! keybindings — in a tiny dependency-free `key = value` format (the same style as `workspaces.conf`),
//! so the core stays free of serde/toml and this parser is fully headless-testable.
//!
//! Unknown keys are ignored (forward-compatible); malformed values fall back to the default. The GUI
//! (`hl-term`) loads this at startup, applies it to every VTE terminal, and live-reloads on file change.

use std::path::{Path, PathBuf};

/// Terminal cursor shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

impl CursorShape {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Beam => "beam",
            CursorShape::Underline => "underline",
        }
    }
    #[must_use]
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
    "#2b2d33", "#ff5f56", "#5af78e", "#f3f99d", "#57c7ff", "#ff6ac1", "#9aedfe", "#c7ccd6", "#5c6370", "#ff6e67",
    "#5af78e", "#f4f99d", "#6dcbff", "#ff92d0", "#a5f0ff", "#ffffff",
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
            palette: DEFAULT_PALETTE.map(std::string::ToString::to_string),
            keybindings: Vec::new(),
        }
    }
}

impl TermConfig {
    /// The default config path, `<hl_root>/term.conf`.
    #[must_use]
    pub fn path(hl_root: &Path) -> PathBuf {
        hl_root.join("term.conf")
    }

    /// The Pango font description string VTE wants, e.g. `"Menlo 12"`.
    #[must_use]
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
    #[must_use]
    pub fn scrollback_lines(&self) -> i64 {
        match self.scrollback {
            None | Some(0) => 10_000_000,
            Some(n) => n as i64,
        }
    }

    /// Look up a keybinding override by action name.
    #[must_use]
    pub fn keybinding(&self, action: &str) -> Option<&str> {
        self.keybindings
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, v)| v.as_str())
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
            self.apply_line(raw);
        }
    }

    fn apply_line(&mut self, raw: &str) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let Some((key, value)) = line.split_once('=') else {
            return;
        };
        self.apply_entry(key.trim(), value.trim());
    }

    fn apply_entry(&mut self, key: &str, value: &str) {
        match key {
            "font" if !value.is_empty() => self.apply_font(value),
            "font_family" if !value.is_empty() => self.font_family = value.to_string(),
            "font_size" | "size" => {
                if let Some(n) = value.parse::<f64>().ok().filter(|n| *n > 0.0) {
                    self.font_size = n;
                }
            }
            "scrollback" => {
                // `unlimited`/`0`/empty → unlimited; a number → cap.
                self.scrollback = match value.to_ascii_lowercase().as_str() {
                    "" | "0" | "unlimited" | "infinite" | "inf" => None,
                    _ => value.parse::<u64>().ok().filter(|n| *n > 0),
                };
            }
            "cursor_shape" | "cursor" => {
                if let Some(cs) = CursorShape::parse(value) {
                    self.cursor_shape = cs;
                }
            }
            "cursor_blink" | "blink" => {
                self.cursor_blink = match value.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    _ => self.cursor_blink,
                };
            }
            "foreground" | "fg" => self.apply_color(value, None),
            "background" | "bg" => self.apply_color(value, Some(16)),
            _ => {
                self.apply_named_entry(key, value);
            }
        }
    }

    fn apply_named_entry(&mut self, key: &str, value: &str) {
        if let Some(index) = key.strip_prefix("color").and_then(|n| n.parse().ok()) {
            self.apply_color(value, Some(index));
            return;
        }
        let Some(action) = key.strip_prefix("key.") else {
            return;
        };
        if action.is_empty() || value.is_empty() {
            return;
        }
        self.keybindings.retain(|(name, _)| name != action);
        self.keybindings.push((action.to_string(), value.to_string()));
    }

    fn apply_color(&mut self, value: &str, palette: Option<usize>) {
        let Some(color) = ConfigColor::parse(value) else {
            return;
        };
        match palette {
            None => self.foreground = color,
            Some(16) => self.background = color,
            Some(index) if index < 16 => self.palette[index] = color,
            Some(_) => {}
        }
    }

    /// Apply the Pango-style `Family Name 13` shorthand used by the `font` config key.
    fn apply_font(&mut self, value: &str) {
        let value = value.trim();
        let parsed = value
            .rsplit_once(char::is_whitespace)
            .and_then(|(family, size)| size.parse::<f64>().ok().map(|size| (family.trim(), size)));
        if let Some((family, size)) = parsed.filter(|(family, size)| !family.is_empty() && *size > 0.0) {
            self.font_family = family.to_string();
            self.font_size = size;
            return;
        }
        self.font_family = value.to_string();
    }

    /// A commented sample config (written on first run so users have something to edit).
    #[must_use]
    pub fn sample() -> String {
        let mut s =
            String::from("# hl terminal config — ~/.hl/term.conf\n# edit + save; open terminals live-reload.\n\n");
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

struct ConfigColor;

impl ConfigColor {
    fn parse(value: &str) -> Option<String> {
        let value = value.trim();
        let body = value.strip_prefix('#').unwrap_or(value);
        if !matches!(body.len(), 3 | 6) || !body.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(if value.starts_with('#') {
            value.to_string()
        } else {
            format!("#{value}")
        })
    }
}

#[cfg(test)]
mod tests {
    // Font sizes are asserted against exactly representable literals the parser just produced.
    #![allow(clippy::float_cmp)]

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
    fn font_combined_form_keeps_family_when_size_is_invalid() {
        let mut c = TermConfig::default();
        c.apply_text("font = Berkeley Mono large\n");
        assert_eq!(c.font_family, "Berkeley Mono large");
        assert_eq!(c.font_size, 12.0);
    }

    #[test]
    fn path_uses_terminal_config_name() {
        assert_eq!(
            TermConfig::path(Path::new("/tmp/hl")),
            PathBuf::from("/tmp/hl/term.conf")
        );
    }

    #[test]
    fn scrollback_unlimited_word() {
        let mut c = TermConfig {
            scrollback: Some(10),
            ..TermConfig::default()
        };
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
        let c = TermConfig::load("/nonexistent/hl/term.conf");
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
        // Start away from the default, then re-apply the sample → back to default values.
        let mut c = TermConfig {
            font_size: 99.0,
            ..TermConfig::default()
        };
        c.apply_text(&sample);
        assert_eq!(c, TermConfig::default());
    }
}
