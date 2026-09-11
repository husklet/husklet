//! Tables: the described one, composed from rows and cells, and the windowed
//! one, driven by a data source.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

const AUTHORED_WIDTH: &str = "hl-tablecell-authored-width";

/// Table components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Table => table().upcast(),
        Tag::TablePagination => pagination().upcast(),
        Tag::TableHead | Tag::TableBody | Tag::TableFooter => group().upcast(),
        Tag::TableRow => line().upcast(),
        Tag::TableCell => cell().upcast(),
        // DataTable, TreeTable and EventStream are the last table tags routed here. They
        // share the column view: the row protocol delivers flat rows of cells
        // with no parent or depth on the wire, so a tree model would have
        // nothing to nest by; the two differ only once the protocol carries
        // hierarchy.
        _ => view().upcast(),
    }
}

fn table() -> gtk::Box {
    let widget = axis::column(0);
    widget.set_hexpand(true);
    widget
}

fn group() -> gtk::Box {
    let widget = axis::column(0);
    widget.set_hexpand(true);
    widget
}

/// One row of cells. Homogeneous, because a column only reads as a column when
/// the cell above and the cell below it start at the same place.
fn line() -> gtk::Box {
    let widget = axis::row(0);
    widget.set_homogeneous(true);
    widget.set_hexpand(true);
    widget
}

/// The strip under a table: which page is shown, and the controls that change
/// it.
///
/// The count is the strip's own value, so a producer sends "21–40 of 240" as
/// `Value`. The page-size selector and the two steps are described children
/// rather than built-in parts, because a widget only reports interaction when
/// it is the widget a node was created for — a button built inside a composite
/// could be clicked and would tell the producer nothing.
fn pagination() -> gtk::Box {
    let widget = axis::row(8);
    let count = axis::label();
    slot::field(&count);
    widget.append(&count);
    widget.set_halign(gtk::Align::End);
    widget.set_hexpand(true);
    widget
}

fn cell() -> gtk::Label {
    let widget = axis::label();
    widget.set_ellipsize(gtk::pango::EllipsizeMode::End);
    widget.set_hexpand(true);
    widget.set_halign(gtk::Align::Fill);
    widget.set_xalign(0.0);
    widget
}

/// A column view over a model the source layer populates. Columns are declared
/// as a property, so construction leaves the view empty on purpose.
fn view() -> gtk::ScrolledWindow {
    let view = gtk::ColumnView::new(None::<gtk::SelectionModel>);
    view.set_reorderable(false);
    view.set_show_row_separators(true);
    view.set_show_column_separators(false);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&view));
    window.set_hexpand(true);
    window.set_vexpand(true);
    window.set_propagate_natural_width(false);
    window.set_min_content_width(0);
    window.set_min_content_height(160);
    let responsive_view = view.clone();
    window.add_tick_callback(move |window, _| {
        crate::collection::responsive(&responsive_view, window.width());
        gtk::glib::ControlFlow::Continue
    });
    window
}

/// The column view behind a windowed table, when the widget is one.
pub(crate) fn columns(widget: &gtk::Widget) -> Option<gtk::ColumnView> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::ColumnView>().ok())
}

/// Places a table's sections: headings above the body whatever order they were
/// described in, summaries below it.
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag, index: usize) -> bool {
    if super::belongs(parent, Tag::TableRow) && tag == Tag::TableCell {
        let Some(row) = parent.downcast_ref::<gtk::Box>() else {
            return false;
        };
        crate::build::insert_into(row, child, index);
        size_row(row);
        align_columns(row);
        return true;
    }
    if !super::belongs(parent, Tag::Table) {
        return false;
    }
    let Some(container) = parent.downcast_ref::<gtk::Box>() else {
        return false;
    };
    match tag {
        Tag::TableHead => container.prepend(child),
        Tag::TableBody => super::precede(container, child, Tag::TableFooter),
        Tag::TableFooter => container.append(child),
        _ => return false,
    }
    for row in table_rows(container) {
        align_columns(&row);
    }
    true
}

/// Marks whether a table cell carries an authored column width. Rows without
/// widths retain equal columns; once a producer describes widths, GTK follows
/// those widths instead of silently replacing them with equal shares.
pub(crate) fn authored_width(widget: &gtk::Widget, value: &hl_gui::PropValue) {
    if !super::belongs(widget, Tag::TableCell) {
        return;
    }
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        match value.as_length() {
            Some(hl_gui::Length::Chars(count)) => {
                label.set_max_width_chars(i32::from(count));
                label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                label.set_hexpand(false);
            }
            None if matches!(value, hl_gui::PropValue::Nothing) => label.set_max_width_chars(-1),
            _ => {}
        }
    }
    let authored = !matches!(value, hl_gui::PropValue::Nothing);
    if authored {
        widget.add_css_class(AUTHORED_WIDTH);
    } else {
        widget.remove_css_class(AUTHORED_WIDTH);
    }
    let Some(row) = widget.parent().and_then(|parent| parent.downcast::<gtk::Box>().ok()) else {
        return;
    };
    size_row(&row);
    align_columns(&row);
}

fn size_row(row: &gtk::Box) {
    let mut child = row.first_child();
    let mut authored = false;
    while let Some(cell) = child {
        child = cell.next_sibling();
        authored |= cell.has_css_class(AUTHORED_WIDTH);
    }
    row.set_homogeneous(!authored);
}

/// Gives independently described rows shared column tracks. `GtkBox` sizes
/// each row in isolation; size groups carry each column's authored width across
/// the header and body without turning unlike columns into equal shares.
fn align_columns(row: &gtk::Box) {
    if row.is_homogeneous() {
        return;
    }
    let Some(table) = row
        .parent()
        .and_then(|section| section.parent())
        .and_then(|table| table.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    let rows = table_rows(&table);
    let column_count = rows.iter().map(children).map(|cells| cells.len()).max().unwrap_or(0);
    let mut groups = Vec::with_capacity(column_count);
    for index in 0..column_count {
        let group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        for cell in rows
            .iter()
            .filter_map(|candidate| children(candidate).get(index).cloned())
        {
            group.add_widget(&cell);
        }
        groups.push(group);
    }
    // A size group holds weak widget references, so the table's own signal
    // closure retains its column groups for exactly as long as the table.
    table.connect_destroy(move |_| drop(groups.clone()));
}

fn table_rows(table: &gtk::Box) -> Vec<gtk::Box> {
    children(table)
        .into_iter()
        .filter_map(|section| section.downcast::<gtk::Box>().ok())
        .flat_map(|section| children(&section))
        .filter_map(|row| row.downcast::<gtk::Box>().ok())
        .filter(|row| row.has_css_class("hl-tablerow"))
        .collect()
}

fn children(container: &gtk::Box) -> Vec<gtk::Widget> {
    let mut children = Vec::new();
    let mut child = container.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        children.push(current);
    }
    children
}
