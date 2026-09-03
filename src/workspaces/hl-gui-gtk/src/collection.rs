//! Collection binding: declared columns and the rows answering a window.

use gtk::prelude::*;
use hl_gui::{Align, Cell, CollectionEdit, Column, Event, Length, Node, Prop, PropValue, RowWindow, SourceId, Trigger};

use crate::rows::{Rows, UNIT};

use crate::component;

/// Nominal advance width of one character in the default interface font.
const CHARACTER_PIXELS: i32 = 9;
/// Horizontal margin applied to each side of a cell.
const CELL_MARGIN: i32 = 8;

/// Applies a collection-shaped property.
pub(crate) fn configure(
    widget: &gtk::Widget,
    node: &Node,
    prop: Prop,
    value: &PropValue,
    reports: &crate::event::Reports,
) {
    match (prop, value) {
        (Prop::Schema, PropValue::Schema(columns)) => schema(widget, node, columns, reports),
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
fn schema(widget: &gtk::Widget, node: &Node, columns: &[Column], reports: &crate::event::Reports) {
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
        view.append_column(&declare(&view, node, column, index, reports));
    }
}

fn declare(
    view: &gtk::ColumnView,
    node: &Node,
    column: &Column,
    index: usize,
    reports: &crate::event::Reports,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let align = column.align;
    let editable = column.editable;
    let view = view.downgrade();
    let reports = reports.clone();
    let node_id = node.id;
    let key = column.key.clone();
    factory.connect_setup(move |_, item| setup(item, align, editable, index, &view, node_id, &key, &reports));
    let title = column.title.clone();
    factory.connect_bind(move |_, item| bind(item, index, &title));
    let declared = gtk::ColumnViewColumn::new(Some(&column.title), Some(factory));
    declared.set_id(Some(&column.key));
    if column.sortable {
        declared.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
    }
    declared.set_resizable(true);
    declared.set_expand(matches!(column.width, Length::Fill));
    if let Length::Chars(count) = column.width {
        // Character width plus the cell's own horizontal margins, or a column
        // sized to its content still clips it.
        declared.set_fixed_width(i32::from(count) * CHARACTER_PIXELS + CELL_MARGIN * 2);
    }
    declared
}

fn setup(
    item: &gtk::glib::Object,
    align: Align,
    editable: bool,
    index: usize,
    view: &gtk::glib::WeakRef<gtk::ColumnView>,
    node: hl_gui::NodeId,
    column: &str,
    reports: &crate::event::Reports,
) {
    let Ok(item) = item.clone().downcast::<gtk::ListItem>() else {
        return;
    };
    if editable {
        let entry = gtk::Entry::new();
        entry.set_has_frame(false);
        entry.set_max_length(Cell::MAX_TEXT_BYTES as i32);
        entry.set_margin_start(CELL_MARGIN);
        entry.set_margin_end(CELL_MARGIN);
        item.set_child(Some(&entry));
        let item = item.downgrade();
        let view = view.clone();
        let column = column.to_owned();
        let reports = reports.clone();
        entry.connect_activate(move |entry| {
            let (Some(item), Some(view), Some(id)) = (item.upgrade(), view.upgrade(), reports.id(node, Trigger::Edit))
            else {
                return;
            };
            let Some(model) = view
                .model()
                .and_then(|m| m.downcast::<gtk::MultiSelection>().ok())
                .and_then(|m| m.model())
                .and_then(|m| m.downcast::<Rows>().ok())
            else {
                return;
            };
            let Some(selection) = model.selection(&[u64::from(item.position())]) else {
                return;
            };
            let Some(row) = selection.rows.into_iter().next() else {
                return;
            };
            let Some(authoritative) = item
                .item()
                .and_then(|item| item.downcast::<gtk::StringObject>().ok())
                .and_then(|item| item.string().split(UNIT).nth(index).map(str::to_owned))
            else {
                return;
            };
            let value = entry.text().to_string();
            if value.len() > Cell::MAX_TEXT_BYTES || value.contains('\0') {
                entry.set_text(&authoritative);
                return;
            }
            reports.push(Event::Edit {
                node,
                id,
                edit: CollectionEdit {
                    source: selection.source,
                    version: selection.version,
                    row,
                    column: column.clone(),
                    value,
                },
            });
            // Edits are proposals, not optimistic authority. Keep the cell
            // controlled by its bound row until the producer publishes an
            // accepted newer source window.
            entry.set_text(&authoritative);
        });
        return;
    }
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

fn bind(item: &gtk::glib::Object, index: usize, title: &str) {
    let Ok(item) = item.clone().downcast::<gtk::ListItem>() else {
        return;
    };
    let (Some(child), Some(entry)) = (item.child(), item.item()) else {
        return;
    };
    let Ok(text) = entry.downcast::<gtk::StringObject>() else {
        return;
    };
    let cells = text.string();
    let value = cells.split(UNIT).nth(index).unwrap_or("");
    if let Ok(label) = child.clone().downcast::<gtk::Label>() {
        label.set_text(value);
    }
    if let Ok(entry) = child.downcast::<gtk::Entry>() {
        entry.set_text(value);
        let label = format!("{title}, row {}", item.position());
        entry.update_property(&[gtk::accessible::Property::Label(&label)]);
        entry.set_tooltip_text(Some(&label));
    }
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
        if existing.source() == source {
            return Some(existing);
        }
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
