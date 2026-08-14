/// Whether one catalogue flag says a tag holds children.
macro_rules! children {
    (children) => {
        true
    };
    ($other:ident) => {
        false
    };
}

/// Whether one catalogue flag says a tag stands free of its parent.
macro_rules! detached {
    (detached) => {
        true
    };
    ($other:ident) => {
        false
    };
}

/// Declares the whole component vocabulary once.
///
/// The enum, the wire spelling, the catalogue order and the two structural
/// questions are all read from this single list, so a tag cannot be added to
/// one of them and forgotten in the others — the failure mode a hand-written
/// `as_str` or `ALL` invites once the library passes a hundred components.
macro_rules! catalogue {
    ($( $tag:ident : $($flag:ident)|+ ),+ $(,)?) => {
        /// The widget kind a node materializes to. One variant per component
        /// contract, grouped by family.
        ///
        /// Toolkit adapters must handle every variant; adding one is a
        /// deliberate widening of the component library, not a rendering hint.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
        pub enum Tag {
            $($tag),+
        }

        impl Tag {
            /// Stable wire spelling. Used by the storybook catalogue and the codec.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$tag => stringify!($tag)),+
                }
            }

            /// Whether this tag accepts children. Leaf tags reject `Insert`
            /// outright so a malformed producer fails at validation rather than
            /// in the adapter.
            #[must_use]
            pub const fn accepts_children(self) -> bool {
                match self {
                    $(Self::$tag => $(children!($flag))||+),+
                }
            }

            /// Whether this tag is a free-standing surface rather than a child
            /// widget. GTK4 has no z-order, so these are attached to the root,
            /// never nested.
            #[must_use]
            pub const fn is_detached(self) -> bool {
                match self {
                    $(Self::$tag => $(detached!($flag))||+),+
                }
            }

            /// Every tag, in catalogue order. The storybook renders this list.
            pub const ALL: &'static [Self] = &[$(Self::$tag),+];
        }
    };
}

catalogue! {
    // Layout: containers and spacing primitives.
    Column: children,
    Row: children,
    Grid: children,
    Scroll: children,
    Splitter: children,
    Stack: children,
    Overlay: children,
    Container: children,
    Spacer: leaf,
    Separator: leaf,

    // Surface: framing and grouping, with the parts a card is composed from.
    Card: children,
    CardHeader: children,
    CardContent: children,
    CardActions: children,
    CardMedia: leaf,
    CardActionArea: children,
    Paper: children,
    Section: children,
    Toolbar: children,
    HeaderBar: children,
    Sidebar: children,

    // Display: text, imagery and status marks.
    Text: leaf,
    Heading: leaf,
    Code: leaf,
    Link: leaf,
    Icon: leaf,
    Badge: leaf,
    Avatar: leaf,
    AvatarGroup: children,
    Chip: children,
    Image: leaf,
    ImageList: children,
    ImageListItem: leaf,

    // Feedback: progress, emptiness and messages.
    Progress: leaf,
    Spinner: leaf,
    Meter: leaf,
    Skeleton: leaf,
    EmptyState: children,
    Stat: children,
    Toast: children,
    Banner: children,
    AlertTitle: leaf,
    InlineMessage: children,

    // Buttons: every shape of invocation.
    Button: children,
    IconButton: children,
    ToggleButton: children,
    ButtonGroup: children,
    ToggleButtonGroup: children,
    SplitButton: children,
    Fab: children,
    SpeedDial: children,
    SpeedDialAction: children,
    Overflow: children,

    // Fields: value entry.
    Entry: leaf,
    Search: leaf,
    NumberEntry: leaf,
    TextArea: leaf,
    PasswordEntry: leaf,
    Autocomplete: leaf,
    TextField: children,
    InputAdornment: children,
    Slider: leaf,
    DatePicker: leaf,
    TimePicker: leaf,
    ColorPicker: leaf,
    FilePicker: leaf,

    // Forms: the frame around a field, and the choice controls.
    FormControl: children,
    FormLabel: leaf,
    FormHelperText: leaf,
    FormControlLabel: children,
    FormGroup: children,
    Switch: leaf,
    Checkbox: leaf,
    Radio: leaf,
    RadioGroup: children,
    Select: leaf,

    // Lists: rows composed from parts.
    List: children,
    ListRow: children,
    ListItemText: leaf,
    ListItemIcon: leaf,
    ListItemAvatar: leaf,
    ListItemButton: children,
    ListItemAction: children,
    ListSubheader: leaf,

    // Tables: the described table and the windowed, source-driven ones.
    Table: children,
    TableHead: children,
    TableBody: children,
    TableFooter: children,
    TableRow: children,
    TableCell: leaf,
    TableSortLabel: leaf,
    DataTable: leaf,
    TreeTable: leaf,

    // Navigation: moving between places and through steps.
    Tabs: children,
    TabPage: children,
    Breadcrumb: children,
    Pagination: children,
    PaginationItem: leaf,
    Stepper: children,
    Step: children,
    StepLabel: children,
    StepContent: children,
    StepConnector: leaf,
    StepIcon: leaf,
    NavigationRail: children,
    NavigationRailItem: leaf,
    BottomNavigation: children,
    BottomNavigationAction: leaf,
    Accordion: children,
    AccordionSummary: children,
    AccordionDetails: children,
    AccordionActions: children,
    Expander: children,

    // Dialogs and transient surfaces.
    Dialog: children | detached,
    DialogTitle: leaf,
    DialogContent: children,
    DialogContentText: leaf,
    DialogActions: children,
    Popover: children | detached,
    ContextMenu: children | detached,
    Menu: children,
    MenuItem: children,

    // Content: long-form text and media.
    CodeView: leaf,
    LogView: leaf,
    Video: leaf,
    Chart: leaf,
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
    fn the_library_is_wide_enough_to_describe_a_real_application() {
        assert!(
            Tag::ALL.len() >= 120,
            "a settings screen, a log viewer and a data browser need parts, not fifty primitives"
        );
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
        assert!(Tag::CardHeader.accepts_children());
    }

    #[test]
    fn only_free_standing_surfaces_are_detached() {
        assert!(Tag::Dialog.is_detached());
        assert!(Tag::Popover.is_detached());
        assert!(Tag::ContextMenu.is_detached());
        assert!(!Tag::DialogContent.is_detached(), "a part lives inside its parent");
        assert!(!Tag::Card.is_detached());
    }
}
