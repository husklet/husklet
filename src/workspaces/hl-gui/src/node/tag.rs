use super::prop::{Prop, Trigger};

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

/// The properties every component honours, whatever it is.
///
/// These are the questions a surrounding layout asks of any child — is it
/// shown, how large is it, where does it sit, what does a pointer reveal — and
/// an adapter answers them on the widget rather than on the component, so
/// naming them per component would be a hundred and thirty repetitions of one
/// fact. A tag's declaration lists only what is true of *that* component;
/// [`Tag::props`] returns these as well, so the contract stays complete.
const EVERY: &[Prop] = &[
    Prop::Visible,
    Prop::Tooltip,
    Prop::Width,
    Prop::Height,
    Prop::Pad,
    Prop::Align,
    Prop::Justify,
    Prop::Grow,
];

/// The properties a component honours only because something places it.
///
/// A cell is decided by the grid holding the child, so these belong to every
/// component that has a parent at all — and to no detached surface, which by
/// definition never sits in one.
const CELL: &[Prop] = &[Prop::Span, Prop::RowSpan];

/// The universal properties, the placement ones, then a component's own.
///
/// A fixed-size array rather than a growable one: the answer is the same for
/// the whole run of the program, so it is computed while compiling and handed
/// out as a borrow of static memory, which is what keeps [`Tag::props`]
/// allocation-free.
const fn joined<const N: usize>(placement: &[Prop], own: &[Prop]) -> [Prop; N] {
    let mut listed = [Prop::Label; N];
    let mut at = poured(&mut listed, 0, EVERY);
    at = poured(&mut listed, at, placement);
    let _ = poured(&mut listed, at, own);
    listed
}

/// Copies one run of properties in, answering where the next run starts.
const fn poured(listed: &mut [Prop], at: usize, source: &[Prop]) -> usize {
    let mut taken = 0;
    while taken < source.len() {
        listed[at + taken] = source[taken];
        taken += 1;
    }
    at + source.len()
}

/// Declares the whole component vocabulary once.
///
/// The enum, the wire spelling, the catalogue order, the two structural
/// questions and the component contract — which properties a component means
/// something by, and which interactions it can report — are all read from this
/// single list, so a tag cannot be added to one of them and forgotten in the
/// others: the failure mode a hand-written `as_str` or `ALL` invites once the
/// library passes a hundred components.
///
/// One entry reads `Tag: structure, props[…], triggers[…]`, where the property
/// list names only what this component means by itself; [`EVERY`] is prepended
/// to it, so the universal layout questions are declared once instead of a
/// hundred and thirty times.
macro_rules! catalogue {
    ($( $tag:ident : $($flag:ident)|+ , props[$($prop:ident),*] , triggers[$($trigger:ident),*] ),+ $(,)?) => {
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

            /// Every property this component honours: the universal ones first,
            /// then its own, in declaration order.
            ///
            /// This is the contract, not a hint. An adapter that accepts one of
            /// these and changes nothing is incomplete, and its conformance test
            /// says so by walking this list.
            #[must_use]
            pub fn props(self) -> &'static [Prop] {
                match self {
                    $(Self::$tag => {
                        const OWN: &[Prop] = &[$(Prop::$prop),*];
                        const PLACED: &[Prop] = if $(detached!($flag))||+ { &[] } else { CELL };
                        const LISTED: [Prop; EVERY.len() + PLACED.len() + OWN.len()] = joined(PLACED, OWN);
                        &LISTED
                    }),+
                }
            }

            /// Every interaction this component can report. A component that
            /// declares none is presentation: binding a handler to it would
            /// leave a producer waiting for an event that never arrives.
            #[must_use]
            pub const fn triggers(self) -> &'static [Trigger] {
                match self {
                    $(Self::$tag => &[$(Trigger::$trigger),*]),+
                }
            }

            /// Whether this component means anything by a property.
            #[must_use]
            pub fn accepts(self, prop: Prop) -> bool {
                self.props().contains(&prop)
            }

            /// Every tag, in catalogue order. The storybook renders this list.
            pub const ALL: &'static [Self] = &[$(Self::$tag),+];
        }
    };
}

catalogue! {
    // Layout: containers and spacing primitives.
    Column: children, props[Gap, Orientation, Wrap], triggers[],
    Row: children, props[Gap, Orientation, Wrap], triggers[],
    Grid: children, props[Gap, Columns], triggers[],
    Scroll: children, props[], triggers[Scroll],
    Splitter: children, props[Orientation, Position], triggers[],
    Stack: children, props[], triggers[],
    Overlay: children, props[], triggers[],
    Container: children, props[Gap], triggers[],
    Spacer: leaf, props[], triggers[],
    Separator: leaf, props[Orientation], triggers[],

    // Surface: framing and grouping, with the parts a card is composed from.
    Card: children, props[Label, Variant, Tone], triggers[],
    CardHeader: children, props[Label, Detail, Icon, Gap], triggers[],
    CardContent: children, props[Gap], triggers[],
    CardActions: children, props[Gap], triggers[],
    CardMedia: leaf, props[Uri], triggers[],
    CardActionArea: children, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke, Focus, Pointer, Context],
    Paper: children, props[Label, Variant, Tone], triggers[],
    Section: children, props[Gap], triggers[],
    Toolbar: children, props[Gap], triggers[],
    HeaderBar: children, props[], triggers[],
    Sidebar: children, props[Gap], triggers[],

    // Display: text, imagery and status marks.
    Text: leaf, props[Label, Value, Wrap, Ellipsize, Scale, Tone, Color], triggers[],
    Heading: leaf, props[Label, Value, Wrap, Ellipsize, Scale, Tone, Color], triggers[],
    Code: leaf, props[Label, Value, Wrap, Ellipsize, Tone, Color], triggers[],
    Link: leaf, props[Label, Uri, Icon, Enabled, Tone], triggers[Invoke, Focus, Pointer, Context],
    Icon: leaf, props[Icon, Tone, Color], triggers[],
    Badge: leaf, props[Label, Value, Tone, Variant], triggers[],
    Avatar: leaf, props[Label, Value, Tone], triggers[],
    AvatarGroup: children, props[Gap], triggers[],
    Chip: children, props[Label, Icon, Gap, Tone, Variant], triggers[],
    Image: leaf, props[Uri], triggers[],
    ImageList: children, props[Gap, Columns], triggers[],
    ImageListItem: leaf, props[Uri], triggers[],

    // Feedback: progress, emptiness and messages.
    Progress: leaf, props[Fraction, Tone], triggers[],
    Spinner: leaf, props[Busy, Tone], triggers[],
    Meter: leaf, props[Fraction, Value, Tone], triggers[],
    Skeleton: leaf, props[], triggers[],
    EmptyState: children, props[Label, Detail, Icon, Gap], triggers[],
    Stat: children, props[Value, Label, Gap, Tone], triggers[],
    Toast: children, props[Label, Icon, Expanded, Tone, Variant], triggers[],
    Banner: children, props[Label, Icon, Expanded, Tone, Variant], triggers[],
    AlertTitle: leaf, props[Label, Value, Scale, Tone], triggers[],
    InlineMessage: children, props[Label, Icon, Gap, Tone], triggers[],

    // Buttons: every shape of invocation.
    Button: children, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke, Key, Focus, Pointer, Context],
    IconButton: children, props[Icon, Label, Enabled, Variant, Tone], triggers[Invoke, Key, Focus, Pointer, Context],
    ToggleButton: children, props[Label, Icon, Checked, Selected, Enabled, Variant, Tone], triggers[Toggle, Invoke, Key, Focus, Pointer, Context],
    ButtonGroup: children, props[Gap, Orientation, Wrap], triggers[],
    ToggleButtonGroup: children, props[Gap, Orientation, Wrap], triggers[],
    SplitButton: children, props[Label, Gap], triggers[],
    Fab: children, props[Icon, Label, Enabled, Variant, Tone], triggers[Invoke],
    SpeedDial: children, props[Icon, Label, Enabled], triggers[],
    SpeedDialAction: children, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    Overflow: children, props[Icon, Label, Enabled], triggers[],

    // Fields: value entry.
    Entry: leaf, props[Value, Placeholder, Secret, Enabled, Tone], triggers[Change, Submit, Key, Focus, Context],
    Search: leaf, props[Value, Placeholder, Enabled], triggers[Change, Submit, Key, Focus, Context],
    NumberEntry: leaf, props[Value, Minimum, Maximum, Step, Enabled], triggers[Change, Key, Focus, Context],
    TextArea: leaf, props[Value, Monospace, Enabled], triggers[Change, Key, Focus, Context],
    PasswordEntry: leaf, props[Value, Placeholder, Secret, Enabled], triggers[Change, Key, Focus, Context],
    Autocomplete: leaf, props[Choices, Enabled], triggers[Change, Select, Key, Focus, Context],
    TextField: children, props[Label, Value, Detail, Placeholder, Gap, Enabled], triggers[],
    InputAdornment: children, props[Label, Gap], triggers[],
    Slider: leaf, props[Value, Minimum, Maximum, Step, Enabled], triggers[Change],
    DatePicker: leaf, props[Value, Enabled], triggers[],
    TimePicker: leaf, props[Value, Gap, Enabled], triggers[],
    ColorPicker: leaf, props[Enabled], triggers[],
    FilePicker: leaf, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    Rating: leaf, props[Value, Enabled], triggers[Change],

    // Forms: the frame around a field, and the choice controls.
    FormControl: children, props[Gap], triggers[],
    FormLabel: leaf, props[Label, Value, Tone], triggers[],
    FormHelperText: leaf, props[Label, Value, Wrap, Tone], triggers[],
    FormControlLabel: children, props[Label, Gap], triggers[],
    FormGroup: children, props[Gap], triggers[],
    Switch: leaf, props[Checked, Selected, Enabled], triggers[Toggle],
    Checkbox: leaf, props[Label, Checked, Selected, Enabled], triggers[Toggle],
    Radio: leaf, props[Label, Checked, Selected, Enabled], triggers[Toggle],
    RadioGroup: children, props[Choices, Gap, Orientation], triggers[],
    Select: leaf, props[Choices, Enabled], triggers[Change, Select, Key, Focus],

    // Lists: rows composed from parts.
    List: children, props[], triggers[],
    ListRow: children, props[Gap], triggers[],
    ListItemText: leaf, props[Label, Detail, Gap], triggers[],
    ListItemIcon: leaf, props[Icon, Tone], triggers[],
    ListItemAvatar: leaf, props[Label, Value, Tone], triggers[],
    ListItemButton: children, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    ListItemAction: children, props[Gap], triggers[],
    ListItemSecondaryAction: children, props[Gap], triggers[],
    ListSubheader: leaf, props[Label, Value, Scale, Tone], triggers[],

    // Tables: the described table and the windowed, source-driven ones.
    Table: children, props[Gap], triggers[],
    TableHead: children, props[Gap], triggers[],
    TableBody: children, props[Gap], triggers[],
    TableFooter: children, props[Gap], triggers[],
    TableRow: children, props[Gap], triggers[],
    TableCell: leaf, props[Label, Value, Ellipsize, Wrap, Tone], triggers[],
    TableSortLabel: leaf, props[Label, Icon, Enabled, Tone], triggers[Invoke],
    DataTable: leaf, props[Schema, Source], triggers[Select, Scroll, Key, Focus, Pointer, Context],
    TreeTable: leaf, props[Schema, Source], triggers[Select, Scroll, Key, Focus, Pointer, Context],
    TablePagination: children, props[Value, Label, Gap], triggers[],

    // Trees: a hierarchy described as nodes rather than windowed as rows.
    Tree: children, props[], triggers[],
    TreeItem: children, props[Label, Expanded], triggers[Expand],

    // Navigation: moving between places and through steps.
    Tabs: children, props[], triggers[],
    TabPage: children, props[Gap], triggers[],
    Breadcrumb: children, props[Gap, Orientation, Wrap], triggers[],
    Pagination: children, props[Gap, Orientation, Wrap], triggers[],
    PaginationItem: leaf, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    Stepper: children, props[Gap, Orientation, Wrap], triggers[],
    Step: children, props[Gap], triggers[],
    StepLabel: children, props[Label, Icon, Gap], triggers[],
    StepContent: children, props[Gap], triggers[],
    StepConnector: leaf, props[Orientation], triggers[],
    StepIcon: leaf, props[Icon, Tone], triggers[],
    NavigationRail: children, props[Gap], triggers[],
    NavigationRailItem: leaf, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    BottomNavigation: children, props[Gap], triggers[],
    BottomNavigationAction: leaf, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    Accordion: children, props[Label, Expanded], triggers[Expand],
    AccordionSummary: children, props[Label, Icon, Gap], triggers[],
    AccordionDetails: children, props[Gap], triggers[],
    AccordionActions: children, props[Gap], triggers[],
    Expander: children, props[Label, Expanded], triggers[Expand],

    // Dialogs and transient surfaces.
    Dialog: children | detached, props[Gap], triggers[],
    DialogTitle: leaf, props[Label, Value, Scale, Tone], triggers[],
    DialogContent: children, props[Gap], triggers[],
    DialogContentText: leaf, props[Label, Value, Wrap, Tone], triggers[],
    DialogActions: children, props[Gap], triggers[],
    Popover: children | detached, props[], triggers[Close],
    ContextMenu: children | detached, props[], triggers[Close],
    Menu: children, props[Gap], triggers[],
    MenuItem: children, props[Label, Icon, Enabled, Variant, Tone], triggers[Invoke],
    Drawer: children, props[], triggers[],
    DrawerPanel: children, props[Expanded, Gap], triggers[],

    // Content: long-form text and media.
    CodeView: leaf, props[Value, Monospace], triggers[],
    LogView: leaf, props[Value, Monospace], triggers[],
    Video: leaf, props[Uri], triggers[],
    Chart: leaf, props[Label, Tone], triggers[],
}

#[cfg(test)]
mod tests {
    use super::{Prop, Tag, Trigger, EVERY};

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

    /// A property named twice would be offered twice by every editor reading
    /// the catalogue, and would be applied twice by a producer walking it.
    #[test]
    fn no_component_declares_a_property_twice() {
        for tag in Tag::ALL {
            let mut seen = std::collections::BTreeSet::new();
            for prop in tag.props() {
                assert!(seen.insert(*prop), "{} declares {prop:?} twice", tag.as_str());
            }
        }
    }

    #[test]
    fn every_component_honours_the_universal_layout_properties() {
        for tag in Tag::ALL {
            for prop in EVERY {
                assert!(tag.accepts(*prop), "{} rejects universal {prop:?}", tag.as_str());
            }
            assert_eq!(
                tag.accepts(Prop::Span),
                !tag.is_detached(),
                "{} answers for a grid cell it cannot sit in",
                tag.as_str()
            );
        }
    }

    #[test]
    fn a_component_declares_what_it_is_for_and_nothing_it_is_not() {
        assert!(Tag::Button.accepts(Prop::Label));
        assert!(!Tag::Button.accepts(Prop::Schema), "a button holds no rows");
        assert!(Tag::DataTable.accepts(Prop::Schema));
        assert!(!Tag::Text.accepts(Prop::Checked), "a label holds no state");
        assert_eq!(Tag::Button.triggers(), &[Trigger::Invoke]);
        assert!(Tag::Text.triggers().is_empty(), "a label reports nothing");
    }
}
