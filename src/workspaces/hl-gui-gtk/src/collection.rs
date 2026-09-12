//! Collection binding: declared columns and the rows answering a window.

use gtk::prelude::*;
use hl_gui::{
    Align, Cell, CollectionEdit, Column, ColumnImportance, Event, Length, Node, Prop, PropValue, RowWindow, SourceId,
    Trigger,
};

use crate::rows::{Rows, UNIT};

use crate::component;

/// Nominal advance width of one character in the default interface font.
const CHARACTER_PIXELS: i32 = 9;
/// Horizontal margin applied to each side of a cell.
const CELL_MARGIN: i32 = 8;
const NARROW_TABLE_PIXELS: i32 = 720;
const TABLE_EDITOR: &str = "hl-table-editor";

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
    let mut optional = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        let declared = declare(&view, node, column, index, reports);
        if column.importance == ColumnImportance::Optional && !column.identity {
            optional.push((declared.clone(), index, column.title.clone()));
        }
        view.append_column(&declared);
    }
    let identity = columns.iter().position(|column| column.identity);
    let details = (!optional.is_empty()).then(|| details_column(&optional, identity));
    if let Some(details) = &details {
        view.append_column(details);
    }
    responsive(&view, widget.width());
}

fn details_column(
    optional: &[(gtk::ColumnViewColumn, usize, String)],
    identity: Option<usize>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let fields = optional
        .iter()
        .map(|(_, index, title)| (*index, title.clone()))
        .collect::<Vec<_>>();
    let indices = fields
        .iter()
        .map(|(index, _)| index.to_string())
        .collect::<Vec<_>>()
        .join(",");
    factory.connect_setup(|_, item| {
        let Ok(item) = item.clone().downcast::<gtk::ListItem>() else {
            return;
        };
        let button = gtk::MenuButton::new();
        button.set_label("Details");
        button.set_focusable(true);
        button.set_halign(gtk::Align::Start);
        button.set_margin_start(CELL_MARGIN);
        button.set_margin_end(CELL_MARGIN);
        let popover = gtk::Popover::new();
        button.set_popover(Some(&popover));
        item.set_child(Some(&button));
    });
    factory.connect_bind(move |_, item| {
        let Ok(item) = item.clone().downcast::<gtk::ListItem>() else {
            return;
        };
        let (Some(child), Some(entry)) = (item.child(), item.item()) else {
            return;
        };
        let (Ok(button), Ok(text)) = (
            child.downcast::<gtk::MenuButton>(),
            entry.downcast::<gtk::StringObject>(),
        ) else {
            return;
        };
        let cells = text.string();
        let values = cells.split(UNIT).collect::<Vec<_>>();
        let mut disclosure = Vec::with_capacity(fields.len());
        for (index, title) in &fields {
            let value = values.get(*index).copied().unwrap_or("");
            let bounded = value.chars().take(256).collect::<String>();
            let suffix = if bounded.len() < value.len() { "…" } else { "" };
            disclosure.push(format!("{title}: {bounded}{suffix}"));
        }
        let content = gtk::Label::new(Some(&disclosure.join("\n")));
        content.set_xalign(0.0);
        content.set_selectable(true);
        content.set_wrap(true);
        content.set_max_width_chars(64);
        content.set_margin_top(8);
        content.set_margin_end(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        let count = fields.len();
        let accessible = format!(
            "{count} hidden field{} for row {}",
            if count == 1 { "" } else { "s" },
            item.position() + 1
        );
        button.set_label(&format!(
            "View {count} field{}",
            if count == 1 { "" } else { "s" }
        ));
        button.set_tooltip_text(Some(&format!("{accessible}: {}", disclosure.join("; "))));
        button.update_property(&[gtk::accessible::Property::Label(&accessible)]);
        if let Some(popover) = button.popover() {
            popover.set_child(Some(&content));
        }
    });
    let declared = gtk::ColumnViewColumn::new(Some("Details"), Some(factory));
    declared.set_id(Some(&format!(
        "__responsive_details:{}:{indices}",
        identity.map_or_else(String::new, |index| index.to_string())
    )));
    declared.set_fixed_width(128);
    declared.set_resizable(false);
    declared.set_visible(false);
    declared
}

/// Applies the responsive visibility policy encoded by the synthetic details
/// column. Called by the one allocation observer owned by the table widget.
pub(crate) fn responsive(view: &gtk::ColumnView, width: i32) {
    let narrow = width > 0 && width <= NARROW_TABLE_PIXELS;
    let columns = (0..view.columns().n_items())
        .filter_map(|index| {
            view.columns()
                .item(index)
                .and_then(|item| item.downcast::<gtk::ColumnViewColumn>().ok())
        })
        .collect::<Vec<_>>();
    let (identity, optional) = columns
        .iter()
        .find_map(|column| {
            column.id().and_then(|id| {
                id.strip_prefix("__responsive_details:").and_then(|metadata| {
                    let (identity, indices) = metadata.split_once(':')?;
                    Some((
                        identity.parse::<usize>().ok(),
                        indices
                            .split(',')
                            .filter_map(|index| index.parse::<usize>().ok())
                            .collect::<Vec<_>>(),
                    ))
                })
            })
        })
        .unwrap_or_default();
    if narrow {
        let hidden_sort = view
            .sorter()
            .and_then(|sorter| sorter.downcast::<gtk::ColumnViewSorter>().ok())
            .and_then(|sorter| sorter.primary_sort_column())
            .and_then(|active| columns.iter().position(|column| column == &active))
            .is_some_and(|index| optional.contains(&index));
        if hidden_sort {
            view.sort_by_column(identity.and_then(|index| columns.get(index)), gtk::SortType::Ascending);
            view.update_property(&[gtk::accessible::Property::Description(
                "Sorting was reset to the identity column because its optional column moved into row details.",
            )]);
        }
    }
    for (index, column) in columns.iter().enumerate() {
        if column.id().is_some_and(|id| id.starts_with("__responsive_details:")) {
            column.set_visible(narrow);
        } else if optional.contains(&index) {
            column.set_visible(!narrow);
        }
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
        // An editable grid is still a grid, not a column of permanently open
        // form fields. The class keeps cells quiet at rest and lets the shared
        // sheet reveal the editing boundary on hover and keyboard focus.
        entry.add_css_class(TABLE_EDITOR);
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
        let label = format!("{title}, row {}", item.position() + 1);
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
