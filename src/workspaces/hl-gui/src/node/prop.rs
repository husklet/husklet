use crate::data::{Column, SourceId};
use crate::style::{Align, Bounds, Edges, Length, Scale, Token, Tone, Variant};

/// One typed property name. A closed set keeps the wire small and lets an
/// adapter be a total function over properties.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Prop {
    // Content
    Label,
    Detail,
    Value,
    Placeholder,
    Help,
    Icon,
    Tooltip,
    Uri,
    // State
    Enabled,
    Visible,
    Selected,
    Checked,
    Expanded,
    Busy,
    Secret,
    /// Whether invoking this node can perform an irreversible operation.
    ///
    /// This is semantic metadata for automation and accessibility consumers;
    /// toolkit adapters deliberately do not infer it from colour or wording.
    Destructive,
    Monospace,
    /// Whether content runs onto further lines instead of overflowing: words
    /// for a text node, whole children for a row or a column.
    Wrap,
    Ellipsize,
    // Appearance
    Variant,
    Tone,
    Scale,
    Color,
    // Layout
    Gap,
    /// Space inside a container's own edges. Takes a [`Length`] for all four
    /// sides or [`PropValue::Edges`] for one, two, or four distinct sides.
    Pad,
    Grow,
    /// Extent along the horizontal axis: a [`Length`], or [`PropValue::Bounds`]
    /// for a floor and ceiling.
    Width,
    /// Extent along the vertical axis, read like [`Prop::Width`].
    Height,
    /// Placement along the container's *main* axis — the axis children advance
    /// on, horizontal for a row and vertical for a column.
    Align,
    /// Placement along the container's *cross* axis, the one children share.
    Justify,
    /// How many columns a grid lays out, or how many a table declares.
    Columns,
    /// How many grid columns this child occupies. Never fewer than one.
    Span,
    /// How many grid rows this child occupies. Never fewer than one.
    RowSpan,
    Orientation,
    Position,
    // Range
    Minimum,
    Maximum,
    Step,
    Fraction,
    // Collection
    Schema,
    Source,
    RowHeight,
    Choices,
}

/// Orientation of a container or divider.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

/// One selectable option offered by a choice control.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A property value. Deliberately narrower than a general document format:
/// no nesting beyond one list level, so an adapter never recurses on a value.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum PropValue {
    Text(String),
    Number(f64),
    Integer(i64),
    Flag(bool),
    Token(Token),
    Length(Length),
    Edges(Edges),
    Bounds(Bounds),
    Variant(Variant),
    Tone(Tone),
    Scale(Scale),
    Align(Align),
    Orientation(Orientation),
    Choices(Vec<Choice>),
    Schema(#[cfg_attr(feature = "wire", serde(deserialize_with = "crate::data::deserialize_columns"))] Vec<Column>),
    Source(SourceId),
    Nothing,
}

impl PropValue {
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_flag(&self) -> Option<bool> {
        match self {
            Self::Flag(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Integer(value) => Some(*value as f64),
            _ => None,
        }
    }

    /// Whether re-sending `other` would tell a renderer nothing new.
    ///
    /// Diffing cannot use `==` alone: `PartialEq` on this type inherits
    /// IEEE-754 equality through `f64`, where `NaN != NaN`. A producer that
    /// describes a number as `NaN` — an unknown ratio, an empty average —
    /// would therefore see the same property re-sent on every single frame,
    /// forever. The arithmetic meaning of `==` is right for everyone else, so
    /// it stays as it is and the diff asks this question instead.
    #[must_use]
    pub fn unchanged(&self, other: &Self) -> bool {
        let (Self::Number(current), Self::Number(next)) = (self, other) else {
            return self == other;
        };
        // A total order answers this without float equality at all: it calls
        // every NaN equal to itself, which is exactly the case `==` gets wrong.
        current.total_cmp(next) == std::cmp::Ordering::Equal
    }

    #[must_use]
    pub fn as_length(&self) -> Option<Length> {
        match self {
            Self::Length(value) => Some(*value),
            _ => None,
        }
    }

    /// Per-side space, reading a plain [`Length`] as the same space on all four
    /// sides so the simple description keeps working unchanged.
    #[must_use]
    pub fn as_edges(&self) -> Option<Edges> {
        match self {
            Self::Edges(value) => Some(*value),
            Self::Length(value) => Some(Edges::all(*value)),
            _ => None,
        }
    }

    /// A size range, reading a plain [`Length`] as an exact size — a floor and
    /// a ceiling at the same place.
    #[must_use]
    pub fn as_bounds(&self) -> Option<Bounds> {
        match self {
            Self::Bounds(value) => Some(*value),
            Self::Length(value) => Some(Bounds::between(*value, *value)),
            _ => None,
        }
    }

    /// A cell count: how many columns a grid has, or how many a child spans.
    ///
    /// Never zero. A child occupying no columns is not a layout a producer can
    /// mean — it would vanish, or in GTK's case be rejected by the container —
    /// so zero and every negative number read as one, and a count too large for
    /// a grid saturates rather than wrapping around.
    #[must_use]
    pub fn as_count(&self) -> Option<u16> {
        let number = self.as_number()?;
        if !number.is_finite() {
            return Some(1);
        }
        let floored = number.floor();
        if floored < 1.0 {
            return Some(1);
        }
        Some(if floored > f64::from(u16::MAX) {
            u16::MAX
        } else {
            floored as u16
        })
    }
}

/// A user interaction kind a node can react to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Trigger {
    Invoke,
    Change,
    Submit,
    Select,
    Edit,
    Sort,
    Activate,
    Toggle,
    Expand,
    Scroll,
    Close,
    Context,
    Key,
    Focus,
    Pointer,
    Drag,
    Drop,
}

/// Stable producer-owned identity for a reaction, echoed back on every event.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct EventId(String);

impl EventId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One declared reaction: a trigger bound to a producer-owned identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Handler {
    pub trigger: Trigger,
    pub id: EventId,
}

impl Handler {
    #[must_use]
    pub fn new(trigger: Trigger, id: EventId) -> Self {
        Self { trigger, id }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventId, Handler, PropValue, Trigger};

    #[test]
    fn integers_read_as_numbers_so_adapters_need_one_path() {
        assert_eq!(PropValue::Integer(7).as_number(), Some(7.0));
        assert_eq!(PropValue::Number(1.5).as_number(), Some(1.5));
        assert_eq!(PropValue::text("x").as_number(), None);
    }

    #[test]
    fn a_number_that_is_not_a_number_still_counts_as_unchanged() {
        let absent = PropValue::Number(f64::NAN);
        assert_ne!(
            absent,
            PropValue::Number(f64::NAN),
            "equality keeps its arithmetic meaning"
        );
        assert!(absent.unchanged(&PropValue::Number(f64::NAN)), "the diff must settle");
        assert!(PropValue::Number(1.5).unchanged(&PropValue::Number(1.5)));
        // A total order separates the signed zeroes. Re-sending one costs a
        // single patch and then settles, which is the harmless direction.
        assert!(!PropValue::Number(0.0).unchanged(&PropValue::Number(-0.0)));
        assert!(!PropValue::Number(1.0).unchanged(&PropValue::Number(2.0)));
        assert!(!absent.unchanged(&PropValue::Number(1.0)));
    }

    #[test]
    fn handlers_keep_typed_identity() {
        let handler = Handler::new(Trigger::Invoke, EventId::new("save"));
        assert_eq!(handler.id.as_str(), "save");
        assert_eq!(handler.trigger, Trigger::Invoke);
    }
}
