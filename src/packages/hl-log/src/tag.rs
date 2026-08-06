//! Typed subsystem tags and tag sets.

use std::convert::Infallible;
use std::fmt::{self, Display, Formatter};
use std::ops::{BitOr, BitOrAssign};
use std::str::FromStr;

/// One registered logging subsystem.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct Tag {
    bits: u64,
    name: &'static str,
}

impl Tag {
    const fn new(bits: u64, name: &'static str) -> Self {
        Self { bits, name }
    }

    /// The stable lowercase configuration and display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The bit assigned to this tag.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.bits
    }
}

impl Display for Tag {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name)
    }
}

/// An unknown subsystem tag name.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ParseTagError;

impl Display for ParseTagError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("unknown log tag")
    }
}

impl std::error::Error for ParseTagError {}

impl FromStr for Tag {
    type Err = ParseTagError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        TAGS.iter()
            .copied()
            .find(|tag| tag.name.eq_ignore_ascii_case(value))
            .ok_or(ParseTagError)
    }
}

/// A set of logging subsystem tags.
#[derive(Copy, Clone, Debug, Default, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct Tags(u64);

impl Tags {
    /// Every tag enabled.
    pub const ALL: Self = Self(!0);
    /// No tags enabled.
    pub const NONE: Self = Self(0);

    /// Construct a set from its raw bit representation.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The raw bit representation used by the atomic runtime gate.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether this set intersects `other`.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// One configured tag list, including names ignored for forward compatibility.
///
/// Keeping both results together ensures diagnostics use the same tokenization and name lookup as the
/// mask itself instead of parsing the environment value a second time with subtly different rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagList<'a> {
    tags: Tags,
    unrecognised: Vec<&'a str>,
}

impl TagList<'_> {
    /// The enabled tag mask represented by this list.
    #[must_use]
    pub const fn tags(&self) -> Tags {
        self.tags
    }

    /// Names ignored while constructing the mask.
    #[must_use]
    pub fn unrecognised(&self) -> &[&str] {
        &self.unrecognised
    }
}

impl<'a> From<&'a str> for TagList<'a> {
    fn from(value: &'a str) -> Self {
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("all") {
            return Self {
                tags: Tags::ALL,
                unrecognised: Vec::new(),
            };
        }
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("off") || trimmed.eq_ignore_ascii_case("none") {
            return Self {
                tags: Tags::NONE,
                unrecognised: Vec::new(),
            };
        }

        let mut tags = Tags::NONE;
        let mut unrecognised = Vec::new();
        for name in trimmed.split([',', '|', ' ']).filter(|name| !name.is_empty()) {
            match name.parse::<Tag>() {
                Ok(tag) => tags |= tag,
                Err(_) => unrecognised.push(name),
            }
        }
        Self { tags, unrecognised }
    }
}

impl From<Tag> for Tags {
    fn from(tag: Tag) -> Self {
        Self(tag.bits)
    }
}

impl From<u64> for Tags {
    fn from(bits: u64) -> Self {
        Self(bits)
    }
}

impl From<Tags> for u64 {
    fn from(tags: Tags) -> Self {
        tags.0
    }
}

impl BitOr for Tag {
    type Output = Tags;

    fn bitor(self, right: Self) -> Self::Output {
        Tags(self.bits | right.bits)
    }
}

impl BitOr<Tag> for Tags {
    type Output = Self;

    fn bitor(self, right: Tag) -> Self::Output {
        Self(self.0 | right.bits)
    }
}

impl BitOr for Tags {
    type Output = Self;

    fn bitor(self, right: Self) -> Self::Output {
        Self(self.0 | right.0)
    }
}

impl BitOrAssign<Tag> for Tags {
    fn bitor_assign(&mut self, right: Tag) {
        self.0 |= right.bits;
    }
}

impl BitOrAssign for Tags {
    fn bitor_assign(&mut self, right: Self) {
        self.0 |= right.0;
    }
}

impl Display for Tags {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        if *self == Self::ALL {
            return formatter.write_str("all");
        }
        if *self == Self::NONE {
            return formatter.write_str("-");
        }

        let mut separator = "";
        for tag in TAGS.iter().filter(|tag| self.0 & tag.bits != 0) {
            formatter.write_str(separator)?;
            formatter.write_str(tag.name)?;
            separator = "|";
        }
        if separator.is_empty() {
            formatter.write_str("?")?;
        }
        Ok(())
    }
}

/// Environment tag lists intentionally ignore unknown names for compatibility.
impl FromStr for Tags {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(TagList::from(value).tags())
    }
}

pub const GPU: Tag = Tag::new(1 << 0, "gpu");
pub const WGPU: Tag = Tag::new(1 << 1, "wgpu");
pub const VULKAN: Tag = Tag::new(1 << 2, "vulkan");
pub const GL: Tag = Tag::new(1 << 3, "gl");
pub const CUDA: Tag = Tag::new(1 << 4, "cuda");
pub const COMPOSITOR: Tag = Tag::new(1 << 5, "compositor");
pub const TRANSPORT: Tag = Tag::new(1 << 6, "transport");
pub const WIRE: Tag = Tag::new(1 << 7, "wire");
pub const PRESENT: Tag = Tag::new(1 << 8, "present");
pub const EXEC: Tag = Tag::new(1 << 9, "exec");
pub const SHIM: Tag = Tag::new(1 << 10, "shim");
pub const RUNTIME: Tag = Tag::new(1 << 11, "runtime");
pub const CPU: Tag = Tag::new(1 << 12, "cpu");
pub const EGL: Tag = Tag::new(1 << 13, "egl");
pub const WAYLAND: Tag = Tag::new(1 << 14, "wayland");
pub const CONTAINER: Tag = Tag::new(1 << 15, "container");
pub const IMAGE: Tag = Tag::new(1 << 16, "image");
pub const DAEMON: Tag = Tag::new(1 << 17, "daemon");
pub const UI: Tag = Tag::new(1 << 18, "ui");
/// Guest syscall admission: decode, route, dispatch, and result write-back.
pub const SYSCALL: Tag = Tag::new(1 << 19, "syscall");
/// Guest filesystem boundaries: path resolution, open, and descriptor installation.
pub const FS: Tag = Tag::new(1 << 20, "fs");
/// Guest network boundaries: socket creation, bind, listen, connect, and accept publication.
pub const NET: Tag = Tag::new(1 << 21, "net");
/// Guest memory boundaries: mapping, protection, faults, and translation-cache invalidation.
pub const MEMORY: Tag = Tag::new(1 << 22, "memory");
/// Guest task lifecycle: fork, thread spawn, exec image replacement, and reaping.
pub const TASK: Tag = Tag::new(1 << 23, "task");
/// Guest signal boundaries: queueing from kill, and the delivery action taken at a safe point.
pub const SIGNAL: Tag = Tag::new(1 << 24, "signal");
/// Guest synchronisation boundaries: futex wait, wake, requeue, and priority-inheritance locking.
pub const SYNC: Tag = Tag::new(1 << 25, "sync");

/// Every tag enabled.
pub const ALL: Tags = Tags::ALL;
/// No tags enabled.
pub const NONE: Tags = Tags::NONE;

/// Registered tags in deterministic display order.
///
/// A new tag is a `Tag::new(1 << <next free bit>, "<name>")` const listed here; parsing,
/// display, and every macro then pick it up automatically.
pub const TAGS: &[Tag] = &[
    GPU, WGPU, VULKAN, GL, CUDA, COMPOSITOR, TRANSPORT, WIRE, PRESENT, EXEC, SHIM, RUNTIME, CPU, EGL, WAYLAND,
    CONTAINER, IMAGE, DAEMON, UI, SYSCALL, FS, NET, MEMORY, TASK, SIGNAL, SYNC,
];

#[cfg(test)]
mod env_value_tests {
    use super::*;
    use std::str::FromStr;

    /// A LEVEL name in the TAG variable opens nothing, and does so silently.
    ///
    /// `gl-diff` shipped `HL_LOG=debug` for as long as the setting existed, beside a comment stating that
    /// the gate was open. `debug` is not a tag, unknown names are dropped for forward compatibility, and
    /// the result is an empty mask indistinguishable from never having asked.
    #[test]
    fn a_level_name_in_the_tag_list_opens_nothing() {
        assert_eq!(Tags::from_str("debug").unwrap(), Tags::NONE);
        assert_eq!(Tags::from_str("warn").unwrap(), Tags::NONE);
    }

    /// The replacement value actually opens the tags a GL context loss is reported under.
    #[test]
    fn the_gl_diff_tag_list_opens_its_tags() {
        let tags = Tags::from_str("gl,egl,present").unwrap();
        assert!(tags.intersects(Tags::from(GL)));
        assert!(tags.intersects(Tags::from(EGL)));
        assert!(tags.intersects(Tags::from(PRESENT)));
        assert!(!tags.intersects(Tags::from(CUDA)));
    }
}

#[cfg(test)]
mod unrecognised_tests {
    use super::*;

    #[test]
    fn a_level_name_is_reported_as_unrecognised() {
        assert_eq!(TagList::from("debug").unrecognised(), &["debug"]);
        assert_eq!(TagList::from("gl,debug,present").unrecognised(), &["debug"]);
    }

    #[test]
    fn a_valid_list_and_the_meanings_report_nothing() {
        assert!(TagList::from("gl,egl,present").unrecognised().is_empty());
        assert!(TagList::from("all").unrecognised().is_empty());
        assert!(TagList::from("off").unrecognised().is_empty());
        assert!(TagList::from("").unrecognised().is_empty());
    }
}
