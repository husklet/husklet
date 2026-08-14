use gtk::prelude::*;
use hl_gui::Tag;

/// Row-oriented components. All of them recycle widgets, so a source with a
/// million rows still costs one viewport of widgets.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::List => list().upcast(),
        Tag::ListRow => row().upcast(),
        Tag::DataTable | Tag::TreeTable => table().upcast(),
        _ => chart().upcast(),
    }
}

fn list() -> gtk::ScrolledWindow {
    let view = gtk::ListBox::new();
    view.set_selection_mode(gtk::SelectionMode::Single);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&view));
    window.set_vexpand(true);
    window
}

fn row() -> gtk::Box {
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    widget.set_hexpand(true);
    widget
}

/// A column view over a model the source layer populates. Columns are declared
/// as a property, so construction leaves the view empty on purpose.
fn table() -> gtk::ScrolledWindow {
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

/// Charts have no toolkit equivalent; the adapter paints them itself.
fn chart() -> gtk::DrawingArea {
    let widget = gtk::DrawingArea::new();
    widget.set_content_height(120);
    widget.set_hexpand(true);
    widget
}

/// The column view behind a table component, when the widget is one.
pub(crate) fn view(widget: &gtk::Widget) -> Option<gtk::ColumnView> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::ColumnView>().ok())
}

/// The list box behind a list component, when the widget is one.
pub(crate) fn rows(widget: &gtk::Widget) -> Option<gtk::ListBox> {
    widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::ListBox>().ok())
}
