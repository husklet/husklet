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
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The bit assigned to this tag.
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
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The raw bit representation used by the atomic runtime gate.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether this set intersects `other`.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
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
        let value = value.trim();
        if value.eq_ignore_ascii_case("all") {
            return Ok(Self::ALL);
        }
        if value.is_empty()
            || value.eq_ignore_ascii_case("off")
            || value.eq_ignore_ascii_case("none")
        {
            return Ok(Self::NONE);
        }

        let mut tags = Self::NONE;
        for name in value.split([',', '|', ' ']) {
            if let Ok(tag) = name.parse::<Tag>() {
                tags |= tag;
            }
        }
        Ok(tags)
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

/// Every tag enabled.
pub const ALL: Tags = Tags::ALL;
/// No tags enabled.
pub const NONE: Tags = Tags::NONE;

/// Registered tags in deterministic display order.
pub const TAGS: &[Tag] = &[
    GPU, WGPU, VULKAN, GL, CUDA, COMPOSITOR, TRANSPORT, WIRE, PRESENT, EXEC, SHIM, RUNTIME, CPU,
    EGL, WAYLAND, CONTAINER, IMAGE, DAEMON, UI,
];
