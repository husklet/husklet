//! Constructors for the components that frame and arrange other components:
//! cards, lists, tables, navigation and dialogs, each with its own parts.

use crate::element::Element;
use crate::node::Tag;

/// Surfaces and the parts a card is composed from.
impl Element {
    /// A width-limited page body.
    #[must_use]
    pub fn container() -> Self {
        Self::new(Tag::Container)
    }

    /// A flat raised surface without a card's parts.
    #[must_use]
    pub fn paper() -> Self {
        Self::new(Tag::Paper)
    }

    /// A card's title row. It lands in the card's own header slot.
    #[must_use]
    pub fn card_header(title: impl Into<String>) -> Self {
        Self::new(Tag::CardHeader).label(title)
    }

    /// The body of a card.
    #[must_use]
    pub fn card_content() -> Self {
        Self::new(Tag::CardContent)
    }

    /// The trailing action row of a card.
    #[must_use]
    pub fn card_actions() -> Self {
        Self::new(Tag::CardActions)
    }

    /// A card's leading picture, named by a file reference.
    #[must_use]
    pub fn card_media(uri: impl Into<String>) -> Self {
        Self::new(Tag::CardMedia).uri(uri)
    }

    /// A card body that is itself invokable.
    #[must_use]
    pub fn card_action_area() -> Self {
        Self::new(Tag::CardActionArea)
    }
}

/// Lists and their row parts.
impl Element {
    /// A scrolling list of rows.
    #[must_use]
    pub fn list() -> Self {
        Self::new(Tag::List)
    }

    /// One row of a list.
    #[must_use]
    pub fn list_row() -> Self {
        Self::new(Tag::ListRow)
    }

    /// The primary and secondary text of a row.
    #[must_use]
    pub fn list_item_text(title: impl Into<String>) -> Self {
        Self::new(Tag::ListItemText).label(title)
    }

    /// The leading icon of a row.
    #[must_use]
    pub fn list_item_icon(icon: impl Into<String>) -> Self {
        Self::new(Tag::ListItemIcon).icon(icon)
    }

    /// The leading monogram of a row.
    #[must_use]
    pub fn list_item_avatar(initials: impl Into<String>) -> Self {
        Self::new(Tag::ListItemAvatar).label(initials)
    }

    /// A row whose whole width is invokable.
    #[must_use]
    pub fn list_item_button(label: impl Into<String>) -> Self {
        Self::new(Tag::ListItemButton).label(label)
    }

    /// The trailing controls of a row.
    #[must_use]
    pub fn list_item_action() -> Self {
        Self::new(Tag::ListItemAction)
    }

    /// A heading between groups of rows.
    #[must_use]
    pub fn list_subheader(label: impl Into<String>) -> Self {
        Self::new(Tag::ListSubheader).label(label)
    }
}

/// Described tables. A table over a data source is [`Element::data_table`].
impl Element {
    /// A table composed from described rows.
    #[must_use]
    pub fn table() -> Self {
        Self::new(Tag::Table)
    }

    /// The header rows of a table.
    #[must_use]
    pub fn table_head() -> Self {
        Self::new(Tag::TableHead)
    }

    /// The body rows of a table.
    #[must_use]
    pub fn table_body() -> Self {
        Self::new(Tag::TableBody)
    }

    /// The summary rows of a table.
    #[must_use]
    pub fn table_footer() -> Self {
        Self::new(Tag::TableFooter)
    }

    /// One row of cells.
    #[must_use]
    pub fn table_row() -> Self {
        Self::new(Tag::TableRow)
    }

    /// One cell.
    #[must_use]
    pub fn table_cell(text: impl Into<String>) -> Self {
        Self::new(Tag::TableCell).label(text)
    }

    /// A column heading that reports the sort it wants.
    #[must_use]
    pub fn table_sort_label(title: impl Into<String>) -> Self {
        Self::new(Tag::TableSortLabel).label(title)
    }

    /// A table over a windowed data source.
    #[must_use]
    pub fn data_table() -> Self {
        Self::new(Tag::DataTable)
    }

    /// Files and directories over a windowed data source.
    #[must_use]
    pub fn file_browser() -> Self {
        Self::new(Tag::FileBrowser)
    }

    /// Property names and values over a windowed data source.
    #[must_use]
    pub fn key_value_table() -> Self {
        Self::new(Tag::KeyValueTable)
    }

    /// A chronological event history over a windowed data source.
    #[must_use]
    pub fn event_stream() -> Self {
        Self::new(Tag::EventStream)
    }
}

/// Navigation, including the parts of a stepper and an accordion.
impl Element {
    /// A trail of ancestor places.
    #[must_use]
    pub fn breadcrumb() -> Self {
        Self::new(Tag::Breadcrumb)
    }

    /// A page selector.
    #[must_use]
    pub fn pagination() -> Self {
        Self::new(Tag::Pagination)
    }

    /// One page number.
    #[must_use]
    pub fn pagination_item(label: impl Into<String>) -> Self {
        Self::new(Tag::PaginationItem).label(label)
    }

    /// A sequence of steps.
    #[must_use]
    pub fn stepper() -> Self {
        Self::new(Tag::Stepper)
    }

    /// One step of a sequence.
    #[must_use]
    pub fn step() -> Self {
        Self::new(Tag::Step)
    }

    /// The caption of a step. It lands in the step's own label slot.
    #[must_use]
    pub fn step_label(label: impl Into<String>) -> Self {
        Self::new(Tag::StepLabel).label(label)
    }

    /// The body of a step.
    #[must_use]
    pub fn step_content() -> Self {
        Self::new(Tag::StepContent)
    }

    /// The rule drawn between two steps.
    #[must_use]
    pub fn step_connector() -> Self {
        Self::new(Tag::StepConnector)
    }

    /// The marker of a step.
    #[must_use]
    pub fn step_icon(icon: impl Into<String>) -> Self {
        Self::new(Tag::StepIcon).icon(icon)
    }

    /// A narrow column of destinations.
    #[must_use]
    pub fn navigation_rail() -> Self {
        Self::new(Tag::NavigationRail)
    }

    /// One destination in a rail.
    #[must_use]
    pub fn navigation_rail_item(label: impl Into<String>) -> Self {
        Self::new(Tag::NavigationRailItem).label(label)
    }

    /// A bar of destinations along the bottom edge.
    #[must_use]
    pub fn bottom_navigation() -> Self {
        Self::new(Tag::BottomNavigation)
    }

    /// One destination in a bottom bar.
    #[must_use]
    pub fn bottom_navigation_action(label: impl Into<String>) -> Self {
        Self::new(Tag::BottomNavigationAction).label(label)
    }

    /// One disclosure of a group, composed from a summary and details.
    #[must_use]
    pub fn accordion() -> Self {
        Self::new(Tag::Accordion)
    }

    /// The always-visible line of an accordion. It lands in its summary slot.
    #[must_use]
    pub fn accordion_summary(label: impl Into<String>) -> Self {
        Self::new(Tag::AccordionSummary).label(label)
    }

    /// The revealed body of an accordion.
    #[must_use]
    pub fn accordion_details() -> Self {
        Self::new(Tag::AccordionDetails)
    }

    /// The action row of an accordion.
    #[must_use]
    pub fn accordion_actions() -> Self {
        Self::new(Tag::AccordionActions)
    }
}

/// Dialogs and transient surfaces.
impl Element {
    /// A free-standing dialog body.
    #[must_use]
    pub fn dialog() -> Self {
        Self::new(Tag::Dialog)
    }

    /// The title of a dialog. It lands at the top of the dialog.
    #[must_use]
    pub fn dialog_title(title: impl Into<String>) -> Self {
        Self::new(Tag::DialogTitle).label(title)
    }

    /// The body of a dialog.
    #[must_use]
    pub fn dialog_content() -> Self {
        Self::new(Tag::DialogContent)
    }

    /// A paragraph of dialog prose.
    #[must_use]
    pub fn dialog_content_text(text: impl Into<String>) -> Self {
        Self::new(Tag::DialogContentText).label(text)
    }

    /// The action row of a dialog. It lands at the bottom of the dialog.
    #[must_use]
    pub fn dialog_actions() -> Self {
        Self::new(Tag::DialogActions)
    }

    /// A menu shown where the pointer asked for it.
    #[must_use]
    pub fn context_menu() -> Self {
        Self::new(Tag::ContextMenu)
    }
}
