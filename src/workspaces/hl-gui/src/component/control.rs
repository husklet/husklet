//! Constructors for the components a person acts on: buttons, fields and the
//! form parts that frame a field.

use crate::element::Element;
use crate::node::{Choice, EventId, Prop, PropValue, Tag, Trigger};

/// Buttons.
impl Element {
    /// A button showing only an icon.
    #[must_use]
    pub fn icon_button(icon: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::IconButton).icon(icon).on(Trigger::Invoke, event)
    }

    /// A button that stays pressed.
    #[must_use]
    pub fn toggle_button(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::ToggleButton).label(label).on(Trigger::Toggle, event)
    }

    /// A run of buttons drawn as one control.
    #[must_use]
    pub fn button_group() -> Self {
        Self::new(Tag::ButtonGroup)
    }

    /// A run of toggles of which one is pressed at a time.
    #[must_use]
    pub fn toggle_button_group() -> Self {
        Self::new(Tag::ToggleButtonGroup)
    }

    /// A default action beside the menu of its alternatives.
    #[must_use]
    pub fn split_button(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::SplitButton).label(label).on(Trigger::Invoke, event)
    }

    /// The one prominent action of a screen.
    #[must_use]
    pub fn floating_action(icon: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::Fab).icon(icon).on(Trigger::Invoke, event)
    }

    /// A button revealing further actions.
    #[must_use]
    pub fn speed_dial(icon: impl Into<String>) -> Self {
        Self::new(Tag::SpeedDial).icon(icon)
    }

    /// One action revealed by a speed dial.
    #[must_use]
    pub fn speed_dial_action(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::SpeedDialAction).label(label).on(Trigger::Invoke, event)
    }

    /// The menu a toolbar puts what it cannot fit into.
    #[must_use]
    pub fn overflow() -> Self {
        Self::new(Tag::Overflow).icon("view-more-symbolic")
    }
}

/// Fields.
impl Element {
    /// A field whose text is hidden, with a control to reveal it.
    #[must_use]
    pub fn password_entry(event: EventId) -> Self {
        Self::new(Tag::PasswordEntry).on(Trigger::Change, event)
    }

    /// A field completing from a fixed list of candidates.
    #[must_use]
    pub fn autocomplete(choices: Vec<Choice>, event: EventId) -> Self {
        Self::new(Tag::Autocomplete)
            .prop(Prop::Choices, PropValue::Choices(choices))
            .on(Trigger::Select, event)
    }

    /// A labelled field with room for helper text and adornments.
    #[must_use]
    pub fn text_field(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::TextField).label(label).on(Trigger::Change, event)
    }

    /// A mark shown inside a field, before or after its value.
    #[must_use]
    pub fn input_adornment(text: impl Into<String>) -> Self {
        Self::new(Tag::InputAdornment).label(text)
    }

    /// A field holding a number.
    #[must_use]
    pub fn number_entry(value: f64, event: EventId) -> Self {
        Self::new(Tag::NumberEntry)
            .prop(Prop::Value, PropValue::Number(value))
            .on(Trigger::Change, event)
    }

    /// A multi-line field.
    #[must_use]
    pub fn text_area(value: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::TextArea).value(value).on(Trigger::Change, event)
    }

    /// A field holding a search term.
    #[must_use]
    pub fn search(event: EventId) -> Self {
        Self::new(Tag::Search).on(Trigger::Change, event)
    }

    /// A searchable command surface whose children describe the available actions.
    #[must_use]
    pub fn command_palette(change: EventId, submit: EventId) -> Self {
        Self::new(Tag::CommandPalette)
            .on(Trigger::Change, change)
            .on(Trigger::Submit, submit)
    }

    /// A multi-value field whose children show the values already retained.
    #[must_use]
    pub fn tag_input(change: EventId, submit: EventId) -> Self {
        Self::new(Tag::TagInput)
            .on(Trigger::Change, change)
            .on(Trigger::Submit, submit)
    }

    /// A calendar date, written as an ISO 8601 day.
    #[must_use]
    pub fn date_picker(day: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::DatePicker).value(day).on(Trigger::Change, event)
    }

    /// A time of day, written as an ISO 8601 hour and minute.
    #[must_use]
    pub fn time_picker(time: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::TimePicker).value(time).on(Trigger::Change, event)
    }

    /// A colour, written as a hexadecimal triple.
    #[must_use]
    pub fn color_picker(event: EventId) -> Self {
        Self::new(Tag::ColorPicker).on(Trigger::Change, event)
    }

    /// A button that asks for a file. The embedder owns the chooser.
    #[must_use]
    pub fn file_picker(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::FilePicker).label(label).on(Trigger::Invoke, event)
    }

    /// A value chosen along a range.
    #[must_use]
    pub fn slider(value: f64, event: EventId) -> Self {
        Self::new(Tag::Slider)
            .prop(Prop::Value, PropValue::Number(value))
            .on(Trigger::Change, event)
    }
}

/// Forms: the frame around a field, and the choice controls.
impl Element {
    /// One field with its label, helper text and control.
    #[must_use]
    pub fn form_control() -> Self {
        Self::new(Tag::FormControl)
    }

    /// The name of a field.
    #[must_use]
    pub fn form_label(label: impl Into<String>) -> Self {
        Self::new(Tag::FormLabel).label(label)
    }

    /// The explanation or error under a field.
    #[must_use]
    pub fn form_helper_text(text: impl Into<String>) -> Self {
        Self::new(Tag::FormHelperText).label(text)
    }

    /// A control with its caption beside it.
    #[must_use]
    pub fn form_control_label(label: impl Into<String>) -> Self {
        Self::new(Tag::FormControlLabel).label(label)
    }

    /// A run of related controls.
    #[must_use]
    pub fn form_group() -> Self {
        Self::new(Tag::FormGroup)
    }

    /// A two-state control.
    #[must_use]
    pub fn switch(checked: bool, event: EventId) -> Self {
        Self::new(Tag::Switch)
            .prop(Prop::Checked, PropValue::Flag(checked))
            .on(Trigger::Toggle, event)
    }

    /// A box that is ticked or not.
    #[must_use]
    pub fn checkbox(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::Checkbox).label(label).on(Trigger::Toggle, event)
    }

    /// One option of a group, exclusive with its siblings.
    #[must_use]
    pub fn radio(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::Radio).label(label).on(Trigger::Toggle, event)
    }

    /// A group of exclusive options.
    #[must_use]
    pub fn radio_group() -> Self {
        Self::new(Tag::RadioGroup)
    }

    /// A closed list of options.
    #[must_use]
    pub fn select(choices: Vec<Choice>, event: EventId) -> Self {
        Self::new(Tag::Select)
            .prop(Prop::Choices, PropValue::Choices(choices))
            .on(Trigger::Select, event)
    }
}
