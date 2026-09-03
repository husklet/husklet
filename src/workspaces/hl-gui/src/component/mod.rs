//! One constructor per component, so a description names a component rather
//! than a tag and a property.
//!
//! The library is wide — a card is a header, a content area and an action row,
//! not a box with three anonymous children — and a producer should be able to
//! say which part it means. Each constructor here is exactly the tag and the
//! properties that part always carries, so a description written with them
//! reduces to the same frame a hand-built one would.

mod content;

pub use content::{
    CoverageLine, CoverageSource, CoverageView, FlameFrame, HexSource, HexView, Instruction, MemoryRegion, TestCase,
    TestStatus, TimelineEvent,
};
mod control;
mod structure;

use crate::element::Element;
use crate::node::{Prop, PropValue};

/// The property shorthands the constructors here are written in terms of.
impl Element {
    /// The node's own value: what a field holds, or what a display shows.
    #[must_use]
    pub fn value(self, value: impl Into<String>) -> Self {
        self.prop(Prop::Value, PropValue::text(value))
    }

    /// Secondary text beside the node's label.
    #[must_use]
    pub fn detail(self, detail: impl Into<String>) -> Self {
        self.prop(Prop::Detail, PropValue::text(detail))
    }

    /// The named icon the node shows.
    #[must_use]
    pub fn icon(self, icon: impl Into<String>) -> Self {
        self.prop(Prop::Icon, PropValue::text(icon))
    }

    /// The file or address the node refers to.
    #[must_use]
    pub fn uri(self, uri: impl Into<String>) -> Self {
        self.prop(Prop::Uri, PropValue::text(uri))
    }

    /// Text shown while the node holds no value.
    #[must_use]
    pub fn placeholder(self, placeholder: impl Into<String>) -> Self {
        self.prop(Prop::Placeholder, PropValue::text(placeholder))
    }
}
