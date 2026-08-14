use gtk::prelude::*;
use hl_gui::Tag;

/// Row-oriented components. All of them recycle widgets, so a source with a
/// million rows still costs one viewport of widgets.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::List => list().upcast(),
        Tag::ListRow => row().upcast(),
        // TreeTable shares the column view. The row protocol delivers flat
        // rows of cells with no parent or depth on the wire, so a tree model
        // would have nothing to nest by; the two differ only once the protocol
        // carries hierarchy.
        Tag::DataTable | Tag::TreeTable => table().upcast(),
        // Chart is the last collection tag, and `build::widget` routes only
        // collection tags here.
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

/// Charts have no toolkit equivalent, so the adapter paints them itself.
///
/// What it paints is a framed, labelled plot area and nothing more. A series
/// is a list of numbers, and the property vocabulary has no such value:
/// `PropValue` carries one number, one length, or a list of label/value
/// *strings*, and the windowed row protocol reaches a `gtk::ColumnView`, not a
/// drawing area. Reading a series out of `Prop::Choices` would widen the
/// meaning of a wire type without widening the type, which is the same change
/// with worse documentation — so the frame is drawn honestly and the data
/// awaits a series value on the wire.
fn chart() -> gtk::DrawingArea {
    let widget = gtk::DrawingArea::new();
    widget.set_content_height(120);
    widget.set_hexpand(true);
    widget.set_draw_func(|area, context, width, height| plot(area, context, f64::from(width), f64::from(height)));
    widget
}

/// Paints the plot frame in the widget's own inherited colour, so the sheet
/// still owns the palette and no per-widget provider is attached.
fn plot(area: &gtk::DrawingArea, context: &gtk::cairo::Context, width: f64, height: f64) {
    let ink = area.color();
    let inset = 8.0_f64;
    context.set_line_width(1.0);
    context.set_source_rgba(ink.red().into(), ink.green().into(), ink.blue().into(), 0.35);
    context.rectangle(
        inset,
        inset,
        (width - inset * 2.0).max(0.0),
        (height - inset * 2.0).max(0.0),
    );
    let _ = context.stroke();
    caption(area, context, width, height);
}

/// The chart's label, centred in the plot area. `Prop::Label` reaches the
/// drawing area as its tooltip — a drawing area holds no text of its own — so
/// that is where the caption is read from.
fn caption(area: &gtk::DrawingArea, context: &gtk::cairo::Context, width: f64, height: f64) {
    let ink = area.color();
    let text = area.tooltip_text().unwrap_or_else(|| "Chart".into());
    context.set_source_rgba(ink.red().into(), ink.green().into(), ink.blue().into(), 0.7);
    context.set_font_size(12.0);
    let Ok(extents) = context.text_extents(&text) else {
        return;
    };
    context.move_to((width - extents.width()) / 2.0, f64::midpoint(height, extents.height()));
    let _ = context.show_text(&text);
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
