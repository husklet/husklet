//! Tables: the described one, composed from rows and cells, and the windowed
//! one, driven by a data source.

use gtk::prelude::*;
use hl_gui::Tag;

use super::{axis, slot};

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
    widget.set_margin_start(8);
    widget.set_margin_end(8);
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
    window.set_min_content_height(160);
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
pub(crate) fn slotted(parent: &gtk::Widget, child: &gtk::Widget, tag: Tag) -> bool {
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
    true
}
