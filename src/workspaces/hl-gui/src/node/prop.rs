use crate::data::{Column, SourceId};
use crate::style::{Align, Length, Scale, Token, Tone, Variant};

/// One typed property name. A closed set keeps the wire small and lets an
/// adapter be a total function over properties.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
    Monospace,
    Wrap,
    Ellipsize,
    // Appearance
    Variant,
    Tone,
    Scale,
    Color,
    // Layout
    Gap,
    Pad,
    Grow,
    Width,
    Height,
    Align,
    Justify,
    Columns,
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
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

/// One selectable option offered by a choice control.
#[derive(Clone, Debug, Eq, PartialEq)]
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
pub enum PropValue {
    Text(String),
    Number(f64),
    Integer(i64),
    Flag(bool),
    Token(Token),
    Length(Length),
    Variant(Variant),
    Tone(Tone),
    Scale(Scale),
    Align(Align),
    Orientation(Orientation),
    Choices(Vec<Choice>),
    Schema(Vec<Column>),
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

    #[must_use]
    pub fn as_length(&self) -> Option<Length> {
        match self {
            Self::Length(value) => Some(*value),
            _ => None,
        }
    }
}

/// A user interaction kind a node can react to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Trigger {
    Invoke,
    Change,
    Submit,
    Select,
    Activate,
    Toggle,
    Expand,
    Scroll,
    Close,
    Context,
}

/// Stable producer-owned identity for a reaction, echoed back on every event.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
    fn handlers_keep_typed_identity() {
        let handler = Handler::new(Trigger::Invoke, EventId::new("save"));
        assert_eq!(handler.id.as_str(), "save");
        assert_eq!(handler.trigger, Trigger::Invoke);
    }
}
