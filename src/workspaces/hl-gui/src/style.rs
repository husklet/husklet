//! Closed styling vocabulary: semantic tokens, a spacing scale, and variants.
//!
//! Nothing here is a color literal or a stylesheet fragment. Producers select
//! meaning; the toolkit adapter owns appearance.

use std::collections::BTreeMap;

/// A semantic color slot resolved by the active theme.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Token {
    Ground,
    Surface,
    Raised,
    Line,
    Text,
    TextDim,
    TextFaint,
    Accent,
    Positive,
    Warning,
    Danger,
    Info,
}

impl Token {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ground => "ground",
            Self::Surface => "surface",
            Self::Raised => "raised",
            Self::Line => "line",
            Self::Text => "text",
            Self::TextDim => "text-dim",
            Self::TextFaint => "text-faint",
            Self::Accent => "accent",
            Self::Positive => "positive",
            Self::Warning => "warning",
            Self::Danger => "danger",
            Self::Info => "info",
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Ground,
        Self::Surface,
        Self::Raised,
        Self::Line,
        Self::Text,
        Self::TextDim,
        Self::TextFaint,
        Self::Accent,
        Self::Positive,
        Self::Warning,
        Self::Danger,
        Self::Info,
    ];
}

/// A spacing or sizing quantity. `Step` is the only numeric spacing on the wire.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Length {
    /// `n` steps on the 4px scale. Clamped to the generated class range.
    Step(u8),
    /// Text-relative width in characters.
    Chars(u16),
    /// Expand to fill the available space.
    Fill,
    /// Natural size of the content.
    Content,
}

impl Length {
    /// Largest step with a generated style class; higher steps clamp to this.
    pub const MAXIMUM_STEP: u8 = 12;
    /// Pixels per step on the spacing scale.
    pub const STEP_PIXELS: u16 = 4;

    #[must_use]
    pub const fn pixels(self) -> Option<u16> {
        match self {
            Self::Step(step) => Some(Self::clamp(step) as u16 * Self::STEP_PIXELS),
            Self::Chars(_) | Self::Fill | Self::Content => None,
        }
    }

    #[must_use]
    pub const fn clamp(step: u8) -> u8 {
        if step > Self::MAXIMUM_STEP {
            Self::MAXIMUM_STEP
        } else {
            step
        }
    }
}

/// Emphasis level of a control.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Variant {
    #[default]
    Plain,
    Filled,
    Outline,
    Ghost,
}

impl Variant {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Filled => "filled",
            Self::Outline => "outline",
            Self::Ghost => "ghost",
        }
    }

    pub const ALL: &'static [Self] = &[Self::Plain, Self::Filled, Self::Outline, Self::Ghost];
}

/// Semantic weight of a control or badge.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Positive,
    Warning,
    Danger,
}

impl Tone {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Accent => "accent",
            Self::Positive => "positive",
            Self::Warning => "warning",
            Self::Danger => "danger",
        }
    }

    pub const ALL: &'static [Self] = &[Self::Neutral, Self::Accent, Self::Positive, Self::Warning, Self::Danger];
}

/// Placement of a child within its allocation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

impl Align {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
            Self::Stretch => "stretch",
        }
    }

    pub const ALL: &'static [Self] = &[Self::Start, Self::Center, Self::End, Self::Stretch];
}

/// Overall compactness of the generated spacing.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

impl Density {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Comfortable => "comfortable",
        }
    }

    pub const ALL: &'static [Self] = &[Self::Compact, Self::Normal, Self::Comfortable];
}

/// Text size role, independent of the concrete font stack.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Scale {
    Caption,
    #[default]
    Body,
    Title,
    Display,
}

impl Scale {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Caption => "caption",
            Self::Body => "body",
            Self::Title => "title",
            Self::Display => "display",
        }
    }

    pub const ALL: &'static [Self] = &[Self::Caption, Self::Body, Self::Title, Self::Display];
}

/// An opaque 24-bit color. The adapter converts to its toolkit representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Lowercase `#rrggbb`, the spelling every toolkit stylesheet accepts.
    #[must_use]
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

/// Resolved appearance for one rendering session.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Theme {
    pub name: String,
    pub colors: BTreeMap<Token, Rgb>,
    pub density: Density,
    pub font: String,
    pub monospace: String,
    pub radius: Length,
}

impl Theme {
    /// A neutral dark palette. Embedding applications are expected to supply
    /// their own; this exists so the library renders standalone.
    #[must_use]
    pub fn dark() -> Self {
        let colors = [
            (Token::Ground, Rgb::new(0x14, 0x15, 0x18)),
            (Token::Surface, Rgb::new(0x1a, 0x1c, 0x20)),
            (Token::Raised, Rgb::new(0x23, 0x26, 0x2b)),
            (Token::Line, Rgb::new(0x2e, 0x32, 0x38)),
            (Token::Text, Rgb::new(0xe6, 0xe8, 0xeb)),
            (Token::TextDim, Rgb::new(0x9a, 0xa0, 0xa8)),
            (Token::TextFaint, Rgb::new(0x6b, 0x71, 0x79)),
            (Token::Accent, Rgb::new(0x4d, 0x9d, 0xff)),
            (Token::Positive, Rgb::new(0x3f, 0xb9, 0x50)),
            (Token::Warning, Rgb::new(0xd2, 0x9a, 0x2c)),
            (Token::Danger, Rgb::new(0xe5, 0x53, 0x53)),
            (Token::Info, Rgb::new(0x6d, 0x8b, 0xa8)),
        ];
        Self {
            name: "dark".into(),
            colors: colors.into_iter().collect(),
            density: Density::Normal,
            font: "Inter, system-ui, sans-serif".into(),
            monospace: "SF Mono, Menlo, monospace".into(),
            radius: Length::Step(2),
        }
    }

    /// Resolved color for a token, falling back to text so a partial theme
    /// renders legibly instead of transparently.
    #[must_use]
    pub fn color(&self, token: Token) -> Rgb {
        self.colors.get(&token).copied().unwrap_or(Rgb::new(0xe6, 0xe8, 0xeb))
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::{Length, Rgb, Theme, Token};

    #[test]
    fn steps_clamp_to_the_generated_class_range() {
        assert_eq!(Length::Step(3).pixels(), Some(12));
        assert_eq!(Length::Step(200).pixels(), Some(48));
        assert_eq!(Length::Fill.pixels(), None);
    }

    #[test]
    fn dark_theme_resolves_every_token() {
        let theme = Theme::dark();
        for token in Token::ALL {
            assert!(theme.colors.contains_key(token), "missing {token:?}");
        }
        assert_eq!(theme.color(Token::Ground), Rgb::new(0x14, 0x15, 0x18));
    }

    #[test]
    fn colors_render_as_lowercase_hex() {
        assert_eq!(Rgb::new(0x4d, 0x9d, 0xff).hex(), "#4d9dff");
    }
}
