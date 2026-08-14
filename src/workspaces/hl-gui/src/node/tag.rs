/// The widget kind a node materializes to. One variant per component contract.
///
/// Toolkit adapters must handle every variant; adding one is a deliberate
/// widening of the component library, not a rendering hint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Tag {
    // Layout
    Column,
    Row,
    Grid,
    Scroll,
    Splitter,
    Stack,
    Overlay,
    Spacer,
    Separator,
    // Display
    Text,
    Heading,
    Code,
    Link,
    Icon,
    Badge,
    Avatar,
    Progress,
    Spinner,
    Image,
    // Control
    Button,
    IconButton,
    ToggleButton,
    Entry,
    Search,
    NumberEntry,
    TextArea,
    Switch,
    Checkbox,
    RadioGroup,
    Select,
    Slider,
    DatePicker,
    ColorPicker,
    FilePicker,
    // Surface
    Card,
    Section,
    Toolbar,
    HeaderBar,
    Sidebar,
    Tabs,
    TabPage,
    Expander,
    Popover,
    Menu,
    MenuItem,
    Dialog,
    Toast,
    Banner,
    // Collection
    List,
    ListRow,
    DataTable,
    TreeTable,
    Chart,
}

impl Tag {
    /// Whether this tag accepts children. Leaf tags reject `Insert` outright so
    /// a malformed producer fails at validation rather than in the adapter.
    #[must_use]
    pub const fn accepts_children(self) -> bool {
        !matches!(
            self,
            Self::Text
                | Self::Heading
                | Self::Code
                | Self::Icon
                | Self::Badge
                | Self::Avatar
                | Self::Progress
                | Self::Spinner
                | Self::Image
                | Self::Spacer
                | Self::Separator
                | Self::Entry
                | Self::Search
                | Self::NumberEntry
                | Self::TextArea
                | Self::Switch
                | Self::Checkbox
                | Self::RadioGroup
                | Self::Select
                | Self::Slider
                | Self::DatePicker
                | Self::ColorPicker
                | Self::FilePicker
                | Self::Link
                | Self::DataTable
                | Self::TreeTable
                | Self::Chart
        )
    }

    /// Whether this tag is a free-standing surface rather than a child widget.
    /// GTK4 has no z-order, so these are attached to the root, never nested.
    #[must_use]
    pub const fn is_detached(self) -> bool {
        matches!(self, Self::Dialog | Self::Popover)
    }

    /// Stable wire spelling. Used by the storybook catalogue and the codec.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Column => "Column",
            Self::Row => "Row",
            Self::Grid => "Grid",
            Self::Scroll => "Scroll",
            Self::Splitter => "Splitter",
            Self::Stack => "Stack",
            Self::Overlay => "Overlay",
            Self::Spacer => "Spacer",
            Self::Separator => "Separator",
            Self::Text => "Text",
            Self::Heading => "Heading",
            Self::Code => "Code",
            Self::Link => "Link",
            Self::Icon => "Icon",
            Self::Badge => "Badge",
            Self::Avatar => "Avatar",
            Self::Progress => "Progress",
            Self::Spinner => "Spinner",
            Self::Image => "Image",
            Self::Button => "Button",
            Self::IconButton => "IconButton",
            Self::ToggleButton => "ToggleButton",
            Self::Entry => "Entry",
            Self::Search => "Search",
            Self::NumberEntry => "NumberEntry",
            Self::TextArea => "TextArea",
            Self::Switch => "Switch",
            Self::Checkbox => "Checkbox",
            Self::RadioGroup => "RadioGroup",
            Self::Select => "Select",
            Self::Slider => "Slider",
            Self::DatePicker => "DatePicker",
            Self::ColorPicker => "ColorPicker",
            Self::FilePicker => "FilePicker",
            Self::Card => "Card",
            Self::Section => "Section",
            Self::Toolbar => "Toolbar",
            Self::HeaderBar => "HeaderBar",
            Self::Sidebar => "Sidebar",
            Self::Tabs => "Tabs",
            Self::TabPage => "TabPage",
            Self::Expander => "Expander",
            Self::Popover => "Popover",
            Self::Menu => "Menu",
            Self::MenuItem => "MenuItem",
            Self::Dialog => "Dialog",
            Self::Toast => "Toast",
            Self::Banner => "Banner",
            Self::List => "List",
            Self::ListRow => "ListRow",
            Self::DataTable => "DataTable",
            Self::TreeTable => "TreeTable",
            Self::Chart => "Chart",
        }
    }

    /// Every tag, in catalogue order. The storybook renders this list.
    pub const ALL: &'static [Self] = &[
        Self::Column,
        Self::Row,
        Self::Grid,
        Self::Scroll,
        Self::Splitter,
        Self::Stack,
        Self::Overlay,
        Self::Spacer,
        Self::Separator,
        Self::Text,
        Self::Heading,
        Self::Code,
        Self::Link,
        Self::Icon,
        Self::Badge,
        Self::Avatar,
        Self::Progress,
        Self::Spinner,
        Self::Image,
        Self::Button,
        Self::IconButton,
        Self::ToggleButton,
        Self::Entry,
        Self::Search,
        Self::NumberEntry,
        Self::TextArea,
        Self::Switch,
        Self::Checkbox,
        Self::RadioGroup,
        Self::Select,
        Self::Slider,
        Self::DatePicker,
        Self::ColorPicker,
        Self::FilePicker,
        Self::Card,
        Self::Section,
        Self::Toolbar,
        Self::HeaderBar,
        Self::Sidebar,
        Self::Tabs,
        Self::TabPage,
        Self::Expander,
        Self::Popover,
        Self::Menu,
        Self::MenuItem,
        Self::Dialog,
        Self::Toast,
        Self::Banner,
        Self::List,
        Self::ListRow,
        Self::DataTable,
        Self::TreeTable,
        Self::Chart,
    ];
}

#[cfg(test)]
mod tests {
    use super::Tag;

    #[test]
    fn catalogue_covers_every_tag_exactly_once() {
        let mut seen = std::collections::BTreeSet::new();
        for tag in Tag::ALL {
            assert!(seen.insert(tag.as_str()), "duplicate catalogue entry {tag:?}");
        }
        assert_eq!(seen.len(), Tag::ALL.len());
    }

    #[test]
    fn leaf_tags_reject_children() {
        assert!(!Tag::Text.accepts_children());
        assert!(!Tag::DataTable.accepts_children());
        assert!(
            Tag::List.accepts_children(),
            "a list is composed from row nodes; only source-driven tables are leaves"
        );
        assert!(Tag::Column.accepts_children());
        assert!(Tag::Card.accepts_children());
    }
}
