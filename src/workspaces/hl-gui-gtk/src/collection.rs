//! Collection binding: declared columns and the rows answering a window.

use gtk::prelude::*;
use hl_gui::{Align, Column, Length, Prop, PropValue, RowWindow, SourceId};

use crate::rows::{Rows, UNIT};

use crate::component;

/// Nominal advance width of one character in the default interface font.
const CHARACTER_PIXELS: i32 = 9;
/// Horizontal margin applied to each side of a cell.
const CELL_MARGIN: i32 = 8;

/// Applies a collection-shaped property.
pub(crate) fn configure(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    match (prop, value) {
        (Prop::Schema, PropValue::Schema(columns)) => schema(widget, columns),
        // Binding a table to a source is what gives it its model: waiting for
        // the first window instead would leave a bound table showing the rows
        // of whatever source it was bound to before.
        (Prop::Source, PropValue::Source(source)) => {
            model(widget, *source);
        }
        _ => {}
    }
}

/// Rebuilds the declared columns of a table.
fn schema(widget: &gtk::Widget, columns: &[Column]) {
    let Some(view) = component::table::columns(widget) else {
        return;
    };
    while let Some(existing) = view.columns().item(0) {
        let Ok(column) = existing.downcast::<gtk::ColumnViewColumn>() else {
            break;
        };
        view.remove_column(&column);
    }
    for (index, column) in columns.iter().enumerate() {
        view.append_column(&declare(column, index));
    }
}

fn declare(column: &Column, index: usize) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let align = column.align;
    factory.connect_setup(move |_, item| setup(item, align));
    factory.connect_bind(move |_, item| bind(item, index));
    let declared = gtk::ColumnViewColumn::new(Some(&column.title), Some(factory));
    declared.set_resizable(true);
    declared.set_expand(matches!(column.width, Length::Fill));
    if let Length::Chars(count) = column.width {
        // Character width plus the cell's own horizontal margins, or a column
        // sized to its content still clips it.
        declared.set_fixed_width(i32::from(count) * CHARACTER_PIXELS + CELL_MARGIN * 2);
    }
    declared
}

fn setup(item: &gtk::glib::Object, align: Align) {
    let Ok(item) = item.clone().downcast::<gtk::ListItem>() else {
        return;
    };
    let label = gtk::Label::new(None);
    label.set_halign(match align {
        Align::Center => gtk::Align::Center,
        Align::End => gtk::Align::End,
        _ => gtk::Align::Start,
    });
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_margin_start(CELL_MARGIN);
    label.set_margin_end(CELL_MARGIN);
    item.set_child(Some(&label));
}

fn bind(item: &gtk::glib::Object, index: usize) {
    let Ok(item) = item.clone().downcast::<gtk::ListItem>() else {
        return;
    };
    let (Some(child), Some(entry)) = (item.child(), item.item()) else {
        return;
    };
    let (Ok(label), Ok(text)) = (child.downcast::<gtk::Label>(), entry.downcast::<gtk::StringObject>()) else {
        return;
    };
    let cells = text.string();
    label.set_text(cells.split(UNIT).nth(index).unwrap_or(""));
}

/// The virtualized model behind a table, created on first use.
///
/// Widget count stays proportional to the viewport rather than the source, so
/// a table over a large result set costs what is on screen.
pub(crate) fn model(widget: &gtk::Widget, source: SourceId) -> Option<Rows> {
    let view = component::table::columns(widget)?;
    if let Some(existing) = view
        .model()
        .and_then(|model| model.downcast::<gtk::MultiSelection>().ok())
        .and_then(|selection| selection.model())
        .and_then(|inner| inner.downcast::<Rows>().ok())
    {
        return Some(existing);
    }
    let rows = Rows::new(source);
    view.set_model(Some(&gtk::MultiSelection::new(Some(rows.clone()))));
    Some(rows)
}

/// Delivers a window to the table bound to its source.
pub(crate) fn present(widget: &gtk::Widget, window: &RowWindow) {
    let Some(rows) = model(widget, window.source) else {
        return;
    };
    // A window can arrive before the length does. Extend only as far as the
    // rows actually delivered: the requested range is what was asked for, not
    // what exists, and trusting it invents placeholder rows past the end.
    let reached = window.range.start.saturating_add(window.rows.len() as u64);
    if u64::from(rows.n_items()) < reached {
        rows.resize(window.version, reached);
    }
    rows.deliver(window);
}

/// Appends a row widget to a list component.
pub(crate) fn append(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(rows) = component::list::rows(parent) else {
        return false;
    };
    rows.append(child);
    true
}
