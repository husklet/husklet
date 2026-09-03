//! Prints the component catalogue as JSON on stdout.
//!
//! A playground written in another language needs the same three answers the
//! library already holds — which components exist, which properties they take,
//! and which values those properties accept — and needs them to change when the
//! library changes. Reading them out of the library at build time is the only
//! arrangement where the two cannot drift: a hand-kept copy in the playground
//! would describe last month's library and nobody would notice.
//!
//! The JSON is written by hand rather than derived. `hl-gui` carries no
//! dependency in its default build, and a catalogue is not a good enough reason
//! to give the whole library one — the document has four object shapes and they
//! are all flat.

use hl_gui::{Align, Density, Prop, Scale, Tag, Token, Tone, Variant};

/// Version of the document shape itself, so a consumer can refuse a catalogue
/// it does not understand instead of reading absent fields as empty ones.
const SHAPE_VERSION: u32 = 1;

/// Largest spacing step with a generated style class, as a length editor should
/// offer it.
const MAXIMUM_STEP: u8 = 12;

fn main() {
    println!("{}", catalogue());
}

/// The whole document.
fn catalogue() -> String {
    let sections = [
        format!("  \"version\": {SHAPE_VERSION}"),
        format!("  \"families\": {}", array(&families(), 2)),
        format!("  \"tags\": {}", array(&tags(), 2)),
        format!("  \"props\": {}", array(&props(), 2)),
        format!("  \"enums\": {}", object(&enumerations(), 2)),
        format!("  \"lengths\": {}", array(&lengths(), 2)),
        format!("  \"notes\": {}", object(&notes(), 2)),
    ];
    format!("{{\n{}\n}}", sections.join(",\n"))
}

/// One entry per component, in catalogue order — the order a sidebar should
/// list them in, since it is the order the library declares them in.
///
/// Each carries the contract the library declares for it: the properties that
/// component means something by, and the interactions it can report. An editor
/// reading this offers a component what it takes and nothing else.
fn tags() -> Vec<String> {
    Tag::ALL
        .iter()
        .map(|tag| {
            format!(
                "{{\"name\": {}, \"family\": {}, \"acceptsChildren\": {}, \"detached\": {}, \"props\": {}, \"triggers\": {}}}",
                text(tag.as_str()),
                text(family(*tag)),
                tag.accepts_children(),
                tag.is_detached(),
                inline(&spelled(tag.props())),
                inline(&spelled(tag.triggers()))
            )
        })
        .collect()
}

/// The wire spelling of each member of a closed vocabulary.
///
/// Read back from `Debug`, which is the derived variant name and therefore
/// exactly what `serde` writes under the `wire` feature, so no name is
/// transcribed here and none can be transcribed wrongly.
fn spelled<T: std::fmt::Debug>(values: &[T]) -> Vec<String> {
    values.iter().map(|value| format!("{value:?}")).collect()
}

/// The families a sidebar groups by, in the order the catalogue declares them.
fn families() -> Vec<String> {
    FAMILIES
        .iter()
        .map(|family| {
            format!(
                "{{\"name\": {}, \"label\": {}, \"note\": {}}}",
                text(family.name),
                text(family.label),
                text(family.note)
            )
        })
        .collect()
}

/// One entry per property, carrying the value shapes it accepts and the editor
/// those shapes ask for.
fn props() -> Vec<String> {
    PROPS
        .iter()
        .map(|entry| {
            format!(
                "{{\"name\": {}, \"group\": {}, \"editor\": {}, \"values\": {}, \"note\": {}}}",
                text(&entry.name()),
                text(entry.group),
                text(entry.editor),
                inline(entry.values),
                text(entry.note)
            )
        })
        .collect()
}

/// The closed enumerations, keyed by the value shape that carries them.
///
/// Each member carries both spellings deliberately: `wire` is what a producer
/// must send, `style` is what the generated stylesheet names the same value, and
/// a playground showing the second while sending the first is the whole point.
fn enumerations() -> Vec<String> {
    vec![
        format!("\"Tone\": {}", inline(&members(Tone::ALL, Tone::as_str))),
        format!("\"Variant\": {}", inline(&members(Variant::ALL, Variant::as_str))),
        format!("\"Scale\": {}", inline(&members(Scale::ALL, Scale::as_str))),
        format!("\"Align\": {}", inline(&members(Align::ALL, Align::as_str))),
        format!("\"Density\": {}", inline(&members(Density::ALL, Density::as_str))),
        format!("\"Token\": {}", inline(&members(Token::ALL, Token::as_str))),
        format!(
            "\"Orientation\": {}",
            inline(&[
                "{\"wire\": \"Horizontal\", \"style\": \"horizontal\"}".to_owned(),
                "{\"wire\": \"Vertical\", \"style\": \"vertical\"}".to_owned()
            ])
        ),
    ]
}

/// The members of one closed enumeration.
///
/// The wire spelling is read back from `Debug`, which is the derived variant
/// name and therefore exactly what `serde` writes under the `wire` feature. It
/// is not transcribed here, so it cannot be transcribed wrongly.
fn members<T: std::fmt::Debug + Copy>(all: &[T], style: fn(T) -> &'static str) -> Vec<String> {
    all.iter()
        .map(|value| {
            format!(
                "{{\"wire\": {}, \"style\": {}}}",
                text(&format!("{value:?}")),
                text(style(*value))
            )
        })
        .collect()
}

/// The shapes a `Length` takes, with what a picker must ask for each.
fn lengths() -> Vec<String> {
    vec![
        format!("{{\"shape\": \"Step\", \"argument\": \"integer\", \"minimum\": 0, \"maximum\": {MAXIMUM_STEP}, \"note\": \"steps on the 4px spacing scale; higher steps clamp\"}}"),
        "{\"shape\": \"Chars\", \"argument\": \"integer\", \"minimum\": 0, \"maximum\": 65535, \"note\": \"text-relative width in characters\"}".to_owned(),
        "{\"shape\": \"Fill\", \"argument\": null, \"note\": \"expand to fill the available space\"}".to_owned(),
        "{\"shape\": \"Content\", \"argument\": null, \"note\": \"natural size of the content\"}".to_owned(),
    ]
}

/// What this document does not know, said plainly, because a playground that
/// guesses is worse than one that offers everything.
fn notes() -> Vec<String> {
    vec![
        format!(
            "\"values\": {}",
            text(
                "A property's value shapes are the PropValue variants an adapter reads for it. \
             Sending another shape is not an error; it is ignored."
            )
        ),
        format!(
            "\"lengthEncoding\": {}",
            text("A Length is an externally tagged enum: {\"Step\": 2}, {\"Chars\": 20}, \"Fill\", \"Content\".")
        ),
        format!(
            "\"logViewRetention\": {}",
            text(&format!(
                "LogView Value patches append; renderers retain only the newest {} Unicode characters.",
                hl_gui::LOG_VIEW_CHARACTER_LIMIT
            ))
        ),
    ]
}

/// One family of components.
struct Family {
    name: &'static str,
    label: &'static str,
    note: &'static str,
}

/// The families the catalogue is written in, taken from the grouping the
/// component list is already divided into.
const FAMILIES: &[Family] = &[
    Family {
        name: "layout",
        label: "Layout",
        note: "containers and spacing primitives",
    },
    Family {
        name: "surface",
        label: "Surface",
        note: "framing and grouping, with the parts a card is composed from",
    },
    Family {
        name: "display",
        label: "Display",
        note: "text, imagery and status marks",
    },
    Family {
        name: "feedback",
        label: "Feedback",
        note: "progress, emptiness and messages",
    },
    Family {
        name: "buttons",
        label: "Buttons",
        note: "every shape of invocation",
    },
    Family {
        name: "fields",
        label: "Fields",
        note: "value entry",
    },
    Family {
        name: "forms",
        label: "Forms",
        note: "the frame around a field, and the choice controls",
    },
    Family {
        name: "lists",
        label: "Lists",
        note: "rows composed from parts",
    },
    Family {
        name: "tables",
        label: "Tables",
        note: "the described table and the windowed, source-driven ones",
    },
    Family {
        name: "trees",
        label: "Trees",
        note: "a hierarchy described as nodes rather than windowed as rows",
    },
    Family {
        name: "navigation",
        label: "Navigation",
        note: "moving between places and through steps",
    },
    Family {
        name: "dialogs",
        label: "Dialogs",
        note: "dialogs and transient surfaces",
    },
    Family {
        name: "content",
        label: "Content",
        note: "long-form text and media",
    },
];

/// The family a component belongs to.
///
/// This match is exhaustive on purpose: adding a component to the library stops
/// this binary compiling until the new component is placed in a family, which is
/// the only way a sidebar stays complete without the library carrying a field it
/// has no other use for.
fn family(tag: Tag) -> &'static str {
    match tag {
        Tag::Column
        | Tag::Row
        | Tag::Grid
        | Tag::Scroll
        | Tag::Splitter
        | Tag::Stack
        | Tag::Overlay
        | Tag::Container
        | Tag::Spacer
        | Tag::Separator => "layout",
        Tag::Card
        | Tag::CardHeader
        | Tag::CardContent
        | Tag::CardActions
        | Tag::CardMedia
        | Tag::CardActionArea
        | Tag::Paper
        | Tag::Section
        | Tag::Toolbar
        | Tag::HeaderBar
        | Tag::Sidebar => "surface",
        Tag::Text
        | Tag::Heading
        | Tag::Code
        | Tag::Link
        | Tag::Icon
        | Tag::Badge
        | Tag::Avatar
        | Tag::AvatarGroup
        | Tag::Chip
        | Tag::Image
        | Tag::ImageList
        | Tag::ImageListItem => "display",
        Tag::Progress
        | Tag::Spinner
        | Tag::Meter
        | Tag::Skeleton
        | Tag::EmptyState
        | Tag::Stat
        | Tag::Toast
        | Tag::Banner
        | Tag::AlertTitle
        | Tag::InlineMessage
        | Tag::ValidationSummary => "feedback",
        Tag::Button
        | Tag::IconButton
        | Tag::ToggleButton
        | Tag::ButtonGroup
        | Tag::ToggleButtonGroup
        | Tag::SplitButton
        | Tag::Fab
        | Tag::SpeedDial
        | Tag::SpeedDialAction
        | Tag::Overflow => "buttons",
        Tag::Entry
        | Tag::Search
        | Tag::CommandPalette
        | Tag::TagInput
        | Tag::NumberEntry
        | Tag::TextArea
        | Tag::PasswordEntry
        | Tag::Autocomplete
        | Tag::TextField
        | Tag::InputAdornment
        | Tag::Slider
        | Tag::DatePicker
        | Tag::TimePicker
        | Tag::ColorPicker
        | Tag::FilePicker
        | Tag::Rating => "fields",
        Tag::FormControl
        | Tag::FormLabel
        | Tag::FormHelperText
        | Tag::FormControlLabel
        | Tag::FormGroup
        | Tag::Switch
        | Tag::Checkbox
        | Tag::Radio
        | Tag::RadioGroup
        | Tag::Select => "forms",
        Tag::List
        | Tag::ListRow
        | Tag::ListItemText
        | Tag::ListItemIcon
        | Tag::ListItemAvatar
        | Tag::ListItemButton
        | Tag::ListItemAction
        | Tag::ListItemSecondaryAction
        | Tag::ListSubheader => "lists",
        Tag::Table
        | Tag::TableHead
        | Tag::TableBody
        | Tag::TableFooter
        | Tag::TableRow
        | Tag::TableCell
        | Tag::TableSortLabel
        | Tag::DataTable
        | Tag::KeyValueTable
        | Tag::TreeTable
        | Tag::EventStream
        | Tag::FileBrowser
        | Tag::TablePagination => "tables",
        Tag::Tree | Tag::TreeItem => "trees",
        Tag::Tabs
        | Tag::TabPage
        | Tag::Breadcrumb
        | Tag::Pagination
        | Tag::PaginationItem
        | Tag::Stepper
        | Tag::Step
        | Tag::StepLabel
        | Tag::StepContent
        | Tag::StepConnector
        | Tag::StepIcon
        | Tag::NavigationRail
        | Tag::NavigationRailItem
        | Tag::BottomNavigation
        | Tag::BottomNavigationAction
        | Tag::Accordion
        | Tag::AccordionSummary
        | Tag::AccordionDetails
        | Tag::AccordionActions
        | Tag::Expander => "navigation",
        Tag::Dialog
        | Tag::DialogTitle
        | Tag::DialogContent
        | Tag::DialogContentText
        | Tag::DialogActions
        | Tag::Popover
        | Tag::ContextMenu
        | Tag::Menu
        | Tag::MenuItem
        | Tag::Drawer
        | Tag::DrawerPanel => "dialogs",
        Tag::CodeView
        | Tag::HexView
        | Tag::MarkdownView
        | Tag::JsonView
        | Tag::LogView
        | Tag::Video
        | Tag::Chart
        | Tag::Sparkline
        | Tag::FlameGraph
        | Tag::MemoryMap
        | Tag::DisassemblyView
        | Tag::TimelineView
        | Tag::TestReportView
        | Tag::CoverageView
        | Tag::NetworkWaterfall
        | Tag::NetworkRequest
        | Tag::NetworkPhase
        | Tag::DependencyGraph
        | Tag::DependencyNode
        | Tag::DependencyEdge
        | Tag::DependencyCycle
        | Tag::DependencyCycleMember
        | Tag::QueryPlan
        | Tag::QueryPlanNode
        | Tag::QueryPlanMetric
        | Tag::DiffViewer
        | Tag::DiffLine => "content",
        Tag::StackTrace | Tag::StackFrame => "content",
    }
}

/// One property, described for an editor.
struct Entry {
    prop: Prop,
    group: &'static str,
    editor: &'static str,
    values: &'static [&'static str],
    note: &'static str,
}

/// Every property, in declaration order.
///
/// The value shapes are read from what the GTK adapter actually does with each
/// property, not from what a property sounds like it should take — an editor
/// offering a shape no adapter reads produces descriptions that render as
/// nothing at all.
const PROPS: &[Entry] = &[
    Entry {
        prop: Prop::Label,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "the node's caption",
    },
    Entry {
        prop: Prop::Detail,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "secondary text beside the label, or a tooltip where there is no room",
    },
    Entry {
        prop: Prop::Value,
        group: "content",
        editor: "text",
        values: &["Text", "Number", "Integer"],
        note: "what a field holds or a display shows; numeric for a slider, a number entry and a rating",
    },
    Entry {
        prop: Prop::Placeholder,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "prompt shown by an empty field",
    },
    Entry {
        prop: Prop::Help,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "explanation revealed by a pointer",
    },
    Entry {
        prop: Prop::Icon,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "named icon",
    },
    Entry {
        prop: Prop::Tooltip,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "explanation revealed by a pointer",
    },
    Entry {
        prop: Prop::Uri,
        group: "content",
        editor: "text",
        values: &["Text"],
        note: "the address a link points at, or the file a picture or a video plays",
    },
    Entry {
        prop: Prop::Enabled,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "defaults to enabled when absent",
    },
    Entry {
        prop: Prop::Visible,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "defaults to visible when absent",
    },
    Entry {
        prop: Prop::Selected,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "read like Checked",
    },
    Entry {
        prop: Prop::Checked,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "for a check button, a toggle button or a switch",
    },
    Entry {
        prop: Prop::Expanded,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "for an expander or a revealed panel",
    },
    Entry {
        prop: Prop::Busy,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "runs a spinner; defaults to busy when absent",
    },
    Entry {
        prop: Prop::Secret,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "hides typed text, and on a password entry withholds the peek icon",
    },
    Entry {
        prop: Prop::Destructive,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "marks an action as irreversible so automation requires confirmation",
    },
    Entry {
        prop: Prop::Monospace,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "for a text view; defaults to monospace when absent",
    },
    Entry {
        prop: Prop::Wrap,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "words onto further lines for text, whole children for a row or a column",
    },
    Entry {
        prop: Prop::Ellipsize,
        group: "state",
        editor: "switch",
        values: &["Flag"],
        note: "for a label",
    },
    Entry {
        prop: Prop::Variant,
        group: "appearance",
        editor: "enum",
        values: &["Variant"],
        note: "emphasis level",
    },
    Entry {
        prop: Prop::Tone,
        group: "appearance",
        editor: "enum",
        values: &["Tone"],
        note: "semantic weight",
    },
    Entry {
        prop: Prop::Scale,
        group: "appearance",
        editor: "enum",
        values: &["Scale"],
        note: "text size role",
    },
    Entry {
        prop: Prop::Color,
        group: "appearance",
        editor: "enum",
        values: &["Token"],
        note: "a semantic color slot, never a literal color",
    },
    Entry {
        prop: Prop::Gap,
        group: "layout",
        editor: "length",
        values: &["Length"],
        note: "space between children; only a Step length has a pixel size",
    },
    Entry {
        prop: Prop::Pad,
        group: "layout",
        editor: "edges",
        values: &["Length", "Edges"],
        note: "a Length applies to all four sides; Edges names them separately",
    },
    Entry {
        prop: Prop::Grow,
        group: "layout",
        editor: "number",
        values: &["Number", "Integer"],
        note: "any value above zero expands the child on both axes",
    },
    Entry {
        prop: Prop::Width,
        group: "layout",
        editor: "length",
        values: &["Length", "Bounds"],
        note: "an exact extent, or a floor and a ceiling",
    },
    Entry {
        prop: Prop::Height,
        group: "layout",
        editor: "length",
        values: &["Length", "Bounds"],
        note: "an exact extent, or a floor and a ceiling",
    },
    Entry {
        prop: Prop::Align,
        group: "layout",
        editor: "enum",
        values: &["Align"],
        note: "placement along the main axis",
    },
    Entry {
        prop: Prop::Justify,
        group: "layout",
        editor: "enum",
        values: &["Align"],
        note: "placement along the cross axis",
    },
    Entry {
        prop: Prop::Columns,
        group: "layout",
        editor: "number",
        values: &["Integer", "Number"],
        note: "grid column count, or a table's declared column count; never below one",
    },
    Entry {
        prop: Prop::Span,
        group: "layout",
        editor: "number",
        values: &["Integer", "Number"],
        note: "grid columns this child occupies; never below one",
    },
    Entry {
        prop: Prop::RowSpan,
        group: "layout",
        editor: "number",
        values: &["Integer", "Number"],
        note: "grid rows this child occupies; never below one",
    },
    Entry {
        prop: Prop::Orientation,
        group: "layout",
        editor: "enum",
        values: &["Orientation"],
        note: "axis of a container, a splitter or a divider",
    },
    Entry {
        prop: Prop::Position,
        group: "layout",
        editor: "number",
        values: &["Number", "Integer"],
        note: "the divider position of a splitter, in pixels",
    },
    Entry {
        prop: Prop::Minimum,
        group: "range",
        editor: "number",
        values: &["Number", "Integer"],
        note: "lower bound of a slider or a number entry",
    },
    Entry {
        prop: Prop::Maximum,
        group: "range",
        editor: "number",
        values: &["Number", "Integer"],
        note: "upper bound of a slider or a number entry",
    },
    Entry {
        prop: Prop::Step,
        group: "range",
        editor: "number",
        values: &["Number", "Integer"],
        note: "increment of a slider or a number entry",
    },
    Entry {
        prop: Prop::Fraction,
        group: "range",
        editor: "number",
        values: &["Number", "Integer"],
        note: "progress or meter fill, clamped to nought through one",
    },
    Entry {
        prop: Prop::Schema,
        group: "collection",
        editor: "schema",
        values: &["Schema"],
        note: "table columns: key, title, width as a Length, align, sortable, editable",
    },
    Entry {
        prop: Prop::Source,
        group: "collection",
        editor: "source",
        values: &["Source"],
        note: "identity of the windowed row source backing a collection",
    },
    Entry {
        prop: Prop::RowHeight,
        group: "collection",
        editor: "number",
        values: &["Number", "Integer"],
        note: "row height of a windowed collection",
    },
    Entry {
        prop: Prop::Choices,
        group: "collection",
        editor: "choices",
        values: &["Choices"],
        note: "options of a select or a radio group, each a value and a label",
    },
];

impl Entry {
    /// The wire spelling of the property this entry describes.
    ///
    /// Read back from `Debug`, which is the derived variant name and therefore
    /// exactly what `serde` writes under the `wire` feature, so no spelling is
    /// transcribed here and none can be transcribed wrongly. The match is
    /// exhaustive so that adding a property stops this binary compiling until
    /// the property is described above.
    fn name(&self) -> String {
        let prop = self.prop;
        match prop {
            Prop::Label
            | Prop::Detail
            | Prop::Value
            | Prop::Placeholder
            | Prop::Help
            | Prop::Icon
            | Prop::Tooltip
            | Prop::Uri
            | Prop::Enabled
            | Prop::Visible
            | Prop::Selected
            | Prop::Checked
            | Prop::Expanded
            | Prop::Busy
            | Prop::Secret
            | Prop::Destructive
            | Prop::Monospace
            | Prop::Wrap
            | Prop::Ellipsize
            | Prop::Variant
            | Prop::Tone
            | Prop::Scale
            | Prop::Color
            | Prop::Gap
            | Prop::Pad
            | Prop::Grow
            | Prop::Width
            | Prop::Height
            | Prop::Align
            | Prop::Justify
            | Prop::Columns
            | Prop::Span
            | Prop::RowSpan
            | Prop::Orientation
            | Prop::Position
            | Prop::Minimum
            | Prop::Maximum
            | Prop::Step
            | Prop::Fraction
            | Prop::Schema
            | Prop::Source
            | Prop::RowHeight
            | Prop::Choices => format!("{prop:?}"),
        }
    }
}

/// A JSON string, with every character JSON forbids raw replaced.
fn text(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    value.chars().for_each(|character| quoted.push_str(&escaped(character)));
    quoted.push('"');
    quoted
}

/// One character as JSON spells it.
fn escaped(character: char) -> String {
    match character {
        '"' => "\\\"".to_owned(),
        '\\' => "\\\\".to_owned(),
        '\n' => "\\n".to_owned(),
        '\r' => "\\r".to_owned(),
        '\t' => "\\t".to_owned(),
        // Every remaining control character has no short spelling, and JSON
        // forbids it raw, so it goes out as its code point.
        control if control < ' ' || control == '\u{7f}' => format!("\\u{:04x}", control as u32),
        other => other.to_string(),
    }
}

/// An array with one item per line, so the document reads in a terminal.
fn array(items: &[String], indent: usize) -> String {
    if items.is_empty() {
        return "[]".to_owned();
    }
    let pad = " ".repeat(indent + 2);
    let separator = format!(",\n{pad}");
    format!("[\n{pad}{}\n{}]", items.join(&separator), " ".repeat(indent))
}

/// An object whose fields are already written, one per line.
fn object(fields: &[String], indent: usize) -> String {
    if fields.is_empty() {
        return "{}".to_owned();
    }
    let pad = " ".repeat(indent + 2);
    let separator = format!(",\n{pad}");
    format!("{{\n{pad}{}\n{}}}", fields.join(&separator), " ".repeat(indent))
}

/// A short array kept on one line, for the handful of values that read worse
/// spread over several.
fn inline<T: AsRef<str>>(items: &[T]) -> String {
    let written: Vec<String> = items.iter().map(|item| quoted(item.as_ref())).collect();
    format!("[{}]", written.join(", "))
}

/// An item of an inline array: already-written JSON passes through, a bare word
/// becomes a string.
fn quoted(item: &str) -> String {
    if item.starts_with('{') {
        return item.to_owned();
    }
    text(item)
}

#[cfg(test)]
mod tests {
    use super::{FAMILIES, PROPS, catalogue, escaped, family, text};
    use hl_gui::Tag;

    /// The document is JSON at all: quotes pair up outside of escapes and no
    /// raw control character survives. Asserted on the string because the
    /// default build has no parser to hand, which is the point of the binary.
    #[test]
    fn the_document_is_well_formed_json_text() {
        let json = catalogue();
        assert!(json.starts_with('{') && json.trim_end().ends_with('}'));
        assert_eq!(quotes(&json) % 2, 0, "an unpaired quote is a truncated document");
        let raw = json.chars().find(|character| *character < ' ' && *character != '\n');
        assert_eq!(raw, None, "a raw control character makes the document unparseable");
        assert_eq!(json.matches('[').count(), json.matches(']').count());
        assert_eq!(json.matches('{').count(), json.matches('}').count());
    }

    /// Unescaped quotes, which a parser would read as string boundaries.
    fn quotes(json: &str) -> usize {
        let mut count = 0;
        let mut escape = false;
        for character in json.chars() {
            let escaped = escape;
            escape = !escaped && character == '\\';
            count += usize::from(character == '"' && !escaped);
        }
        count
    }

    /// The catalogue cannot silently fall behind the library.
    #[test]
    fn every_component_is_described_exactly_once() {
        let json = catalogue();
        let count = json.matches("\"acceptsChildren\"").count();
        assert_eq!(count, Tag::ALL.len(), "one entry per tag, no more and no fewer");
        for tag in Tag::ALL {
            let entry = format!("\"name\": \"{}\", \"family\"", tag.as_str());
            assert!(json.contains(&entry), "missing {tag:?}");
        }
    }

    /// A component declaring a property this document does not describe would
    /// leave an editor with a name and no way to edit it.
    #[test]
    fn every_declared_property_is_described_in_the_property_table() {
        for tag in Tag::ALL {
            for prop in tag.props() {
                assert!(
                    PROPS.iter().any(|entry| entry.prop == *prop),
                    "{tag:?} declares {prop:?}, which the property table does not describe"
                );
            }
        }
    }

    /// A family a sidebar has no heading for would hide its components.
    #[test]
    fn every_component_lands_in_a_declared_family() {
        for tag in Tag::ALL {
            let name = family(*tag);
            assert!(
                FAMILIES.iter().any(|declared| declared.name == name),
                "{tag:?} claims undeclared family {name}"
            );
        }
    }

    /// Two entries for one property would give an editor two answers.
    #[test]
    fn every_property_is_described_once_and_takes_a_value() {
        let json = catalogue();
        assert_eq!(json.matches("\"editor\"").count(), PROPS.len());
        for entry in PROPS {
            assert!(!entry.values.is_empty(), "{:?} accepts nothing", entry.prop);
        }
    }

    /// The enumerations an editor offers as dropdowns are all present.
    #[test]
    fn the_closed_enumerations_are_all_offered() {
        let json = catalogue();
        for name in ["Tone", "Variant", "Scale", "Align", "Density", "Token", "Orientation"] {
            assert!(json.contains(&format!("\"{name}\": [")), "missing enumeration {name}");
        }
        for shape in ["Step", "Chars", "Fill", "Content"] {
            assert!(
                json.contains(&format!("\"shape\": \"{shape}\"")),
                "missing length {shape}"
            );
        }
    }

    #[test]
    fn awkward_characters_survive_quoting() {
        assert_eq!(text("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(text("line\nbreak\ttab"), "\"line\\nbreak\\ttab\"");
        assert_eq!(escaped('\u{1}'), "\\u0001");
        assert_eq!(escaped('\u{7f}'), "\\u007f");
        assert_eq!(
            text("héllo"),
            "\"héllo\"",
            "text outside ASCII needs no escape in UTF-8"
        );
    }
}
