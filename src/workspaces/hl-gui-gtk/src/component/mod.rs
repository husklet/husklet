//! One module per component family, holding every widget of that family and
//! the way its parts are placed.
//!
//! The library is composed rather than configured: a card is a header, a body
//! and an action row, and a list row is a mark, its text and its controls. Each
//! part is its own tag, so the family that owns the parent also owns where its
//! parts land — which is why the placement rules live beside the builders
//! rather than in one table that would have to know every family at once.

pub(crate) mod axis;
pub(crate) mod button;
pub(crate) mod card;
pub(crate) mod content;
pub(crate) mod dialog;
pub(crate) mod drawer;
pub(crate) mod feedback;
pub(crate) mod field;
pub(crate) mod form;
pub(crate) mod list;
pub(crate) mod navigation;
pub(crate) mod slot;
pub(crate) mod table;
pub(crate) mod text;
pub(crate) mod tree;

use gtk::prelude::*;
use hl_gui::Tag;

/// Builds the widget for a tag, dispatching to the family that owns it.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Card
        | Tag::CardHeader
        | Tag::CardContent
        | Tag::CardActions
        | Tag::CardMedia
        | Tag::CardActionArea
        | Tag::AccordionActions
        | Tag::Paper
        | Tag::Container
        | Tag::Section
        | Tag::Toolbar
        | Tag::HeaderBar
        | Tag::Sidebar => card::widget(tag),
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
        | Tag::ImageListItem
        | Tag::ListSubheader
        | Tag::ListItemIcon
        | Tag::ListItemAvatar
        | Tag::StepIcon
        | Tag::FormLabel => text::widget(tag),
        Tag::Progress
        | Tag::Spinner
        | Tag::Meter
        | Tag::Skeleton
        | Tag::EmptyState
        | Tag::Stat
        | Tag::Toast
        | Tag::Banner
        | Tag::AlertTitle
        | Tag::InlineMessage => feedback::widget(tag),
        Tag::Button
        | Tag::IconButton
        | Tag::ToggleButton
        | Tag::ButtonGroup
        | Tag::ToggleButtonGroup
        | Tag::SplitButton
        | Tag::Fab
        | Tag::SpeedDial
        | Tag::SpeedDialAction
        | Tag::Overflow
        | Tag::MenuItem
        | Tag::PaginationItem
        | Tag::TableSortLabel
        | Tag::FilePicker => button::widget(tag),
        Tag::Entry
        | Tag::Search
        | Tag::NumberEntry
        | Tag::TextArea
        | Tag::PasswordEntry
        | Tag::Autocomplete
        | Tag::TextField
        | Tag::InputAdornment
        | Tag::Slider
        | Tag::Rating
        | Tag::DatePicker
        | Tag::TimePicker
        | Tag::ColorPicker => field::widget(tag),
        Tag::FormControl
        | Tag::FormGroup
        | Tag::FormHelperText
        | Tag::FormControlLabel
        | Tag::Switch
        | Tag::Checkbox
        | Tag::Radio
        | Tag::RadioGroup
        | Tag::Select => form::widget(tag),
        Tag::List
        | Tag::ListRow
        | Tag::ListItemText
        | Tag::ListItemButton
        | Tag::ListItemAction
        | Tag::ListItemSecondaryAction => list::widget(tag),
        Tag::Tree | Tag::TreeItem => tree::widget(tag),
        Tag::Drawer | Tag::DrawerPanel => drawer::widget(tag),
        Tag::Table
        | Tag::TablePagination
        | Tag::TableHead
        | Tag::TableBody
        | Tag::TableFooter
        | Tag::TableRow
        | Tag::TableCell
        | Tag::DataTable
        | Tag::TreeTable => table::widget(tag),
        Tag::Tabs
        | Tag::TabPage
        | Tag::Breadcrumb
        | Tag::Pagination
        | Tag::Stepper
        | Tag::Step
        | Tag::StepLabel
        | Tag::StepContent
        | Tag::StepConnector
        | Tag::NavigationRail
        | Tag::NavigationRailItem
        | Tag::BottomNavigation
        | Tag::BottomNavigationAction
        | Tag::Accordion
        | Tag::AccordionSummary
        | Tag::AccordionDetails
        | Tag::Expander => navigation::widget(tag),
        Tag::Dialog
        | Tag::DialogTitle
        | Tag::DialogContent
        | Tag::DialogContentText
        | Tag::DialogActions
        | Tag::Popover
        | Tag::ContextMenu
        | Tag::Menu => dialog::widget(tag),
        // The content family is last, and the layout tags never reach here:
        // `build::widget` routes those to the layout builder before asking.
        _ => content::widget(tag),
    }
}

/// Whether a widget was built for a given tag.
///
/// The per-tag style class is the adapter's own naming of what a widget is, and
/// it is stamped on at construction — so a parent can answer what it is without
/// the registry being consulted or a second table being kept.
pub(crate) fn belongs(widget: &gtk::Widget, tag: Tag) -> bool {
    widget.has_css_class(&crate::build::class(tag))
}

/// Appends a child, but keeps it above the part its parent always ends with.
///
/// A body described after the action row still belongs above it: the order the
/// parts were described in is the producer's convenience, and the order they
/// are read in is the component's contract.
pub(crate) fn precede(container: &gtk::Box, child: &gtk::Widget, trailing: Tag) {
    let Some(last) = slot::offspring(container.upcast_ref())
        .into_iter()
        .find(|held| belongs(held, trailing))
    else {
        container.append(child);
        return;
    };
    container.insert_child_after(child, last.prev_sibling().as_ref());
}

/// Places a child in its parent, in the slot the parent keeps for it.
pub(crate) fn attach(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag, index: usize) -> bool {
    part(parent, child, tag, index) || surface(parent, child, tag, index)
}

/// Placement a parent decides from what its child *is*.
fn part(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag, index: usize) -> bool {
    // The tree and the drawer are asked first: both are built from widgets
    // another family also uses — a disclosure, a revealer — and the family that
    // owns the parent is the one that knows where the child belongs.
    tree::slotted(parent, child)
        || drawer::slotted(parent, child, tag)
        || card::slotted(parent, child, tag)
        || navigation::slotted(parent, child, tag)
        || dialog::slotted(parent, child, tag)
        || table::slotted(parent, child, tag)
        || list::slotted(parent, child, tag)
        || form::slotted(parent, child, tag)
        || field::slotted(parent, child, tag, index)
}

/// Placement a parent decides from the container protocol it implements.
fn surface(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag, index: usize) -> bool {
    feedback::attach(parent, child, tag)
        || card::attach(parent, child)
        || navigation::attach(parent, child, index)
        || dialog::attach(parent, child)
        || text::attach(parent, child)
        || button::attach(parent, child)
}

/// Removes a child from whichever component family holds it.
pub(crate) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    card::detach(parent, child)
        || navigation::detach(parent, child)
        || text::detach(parent, child)
        || dialog::detach(parent, child)
        || button::detach(parent, child)
}
