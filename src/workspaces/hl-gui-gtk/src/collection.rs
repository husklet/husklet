//! Collection binding: declared columns and the rows answering a window.

use gtk::prelude::*;
use hl_gui::{Align, Cell, Column, Length, Prop, PropValue, Row, RowWindow, Tone};

use crate::build;

/// Nominal advance width of one character in the default interface font.
const CHARACTER_PIXELS: i32 = 9;
/// Horizontal margin applied to each side of a cell.
const CELL_MARGIN: i32 = 8;

/// Applies a collection-shaped property.
pub(crate) fn configure(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    match (prop, value) {
        (Prop::Schema, PropValue::Schema(columns)) => schema(widget, columns),
        (Prop::RowHeight, _) => {}
        _ => {}
    }
}

/// Rebuilds the declared columns of a table.
fn schema(widget: &gtk::Widget, columns: &[Column]) {
    let Some(view) = build::collection::view(widget) else {
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
    label.set_text(cells.split('\u{1f}').nth(index).unwrap_or(""));
}

/// Binds a delivered window of rows to a table.
///
/// Rows arrive as unit-separated fields rather than a bespoke list model, so a
/// window lands in one model swap; the windowing cache decides what is asked
/// for in the first place.
pub(crate) fn present(widget: &gtk::Widget, window: &RowWindow) {
    let Some(view) = build::collection::view(widget) else {
        return;
    };
    let encoded: Vec<String> = window.rows.iter().map(encode).collect();
    let borrowed: Vec<&str> = encoded.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&borrowed);
    view.set_model(Some(&gtk::NoSelection::new(Some(model))));
}

fn encode(row: &Row) -> String {
    row.cells.iter().map(render).collect::<Vec<_>>().join("\u{1f}")
}

fn render(cell: &Cell) -> String {
    match cell {
        Cell::Text(value) => value.clone(),
        Cell::Number(value) => format!("{value}"),
        Cell::Bytes(value) => hl_gui::ByteSize::new(*value as i64).to_string(),
        Cell::Badge { label, tone } => badge(label, *tone),
        Cell::Stamp(value) => format!("{value}"),
        Cell::Empty => "—".into(),
    }
}

fn badge(label: &str, tone: Tone) -> String {
    match tone {
        Tone::Positive => format!("● {label}"),
        Tone::Warning => format!("▲ {label}"),
        Tone::Danger => format!("✕ {label}"),
        _ => label.into(),
    }
}

/// Appends a row widget to a list component.
pub(crate) fn append(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    let Some(rows) = build::collection::rows(parent) else {
        return false;
    };
    rows.append(child);
    true
}
