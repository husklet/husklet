use std::cell::RefCell;
use std::collections::BTreeSet;

use gtk::prelude::*;
use hl_gui::{Align, Orientation, Prop, PropValue, Tag};

pub(crate) use super::flow::Flow;

/// Containers and spacing primitives.
pub(super) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::Column => gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
        Tag::Row => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
        Tag::Grid => gtk::Grid::new().upcast(),
        Tag::Scroll => scroll().upcast(),
        Tag::Splitter => gtk::Paned::new(gtk::Orientation::Horizontal).upcast(),
        Tag::Stack => stack().upcast(),
        Tag::Overlay => gtk::Overlay::new().upcast(),
        Tag::Spacer => spacer().upcast(),
        // Separator is the last layout tag, and `build::widget` routes only
        // layout tags here, so this arm is that tag. It stays a catch-all
        // because `Tag` is one enum for every family and a family builder
        // cannot name the other fifty variants.
        _ => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    }
}

/// One page visible at a time. The stack expands so a page is laid out at the
/// size the stack was given rather than at the size of the widest page.
fn stack() -> gtk::Stack {
    let widget = gtk::Stack::new();
    widget.set_hexpand(true);
    widget.set_vexpand(true);
    widget
}

fn scroll() -> gtk::ScrolledWindow {
    let window = gtk::ScrolledWindow::new();
    window.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    window.set_hexpand(true);
    window.set_vexpand(true);
    window
}

/// A blank box that consumes leftover horizontal space. Expanding on both axes
/// would stretch every toolbar it sits in; a vertical spacer asks for a height.
fn spacer() -> gtk::Box {
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    widget.set_hexpand(true);
    widget
}

/// Attaches to the layout containers whose protocol is not `gtk::Box`.
pub(super) fn attach(parent: &gtk::Widget, child: &gtk::Widget, index: usize) -> bool {
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        if index == 0 {
            paned.set_start_child(Some(child));
        } else {
            paned.set_end_child(Some(child));
        }
        return true;
    }
    if let Some(stack) = parent.downcast_ref::<gtk::Stack>() {
        stack.add_named(child, Some(&format!("page-{index}")));
        return true;
    }
    if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
        if index == 0 {
            overlay.set_child(Some(child));
        } else {
            overlay.add_overlay(child);
        }
        return true;
    }
    if let Some(grid) = parent.downcast_ref::<gtk::Grid>() {
        seat(grid, child, index);
        return true;
    }
    false
}

/// Removes a child from the layout containers this module attaches to.
///
/// These containers keep their own page or slot bookkeeping, so unparenting a
/// child behind their back leaves a named page pointing at nothing — which is
/// exactly what makes a later move place the child twice.
pub(super) fn detach(parent: &gtk::Widget, child: &gtk::Widget) -> bool {
    if let Some(stack) = parent.downcast_ref::<gtk::Stack>() {
        stack.remove(child);
        return true;
    }
    if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
        return uncover(overlay, child);
    }
    if let Some(grid) = parent.downcast_ref::<gtk::Grid>() {
        grid.remove(child);
        // The departed child left a hole in the flow, and every child after it
        // moves up one cell — the same rule that placed them in the first place.
        arrange(grid, &offspring(grid));
        return true;
    }
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        return unpane(paned, child);
    }
    false
}

fn uncover(overlay: &gtk::Overlay, child: &gtk::Widget) -> bool {
    if overlay.child().is_some_and(|held| held.eq(child)) {
        overlay.set_child(gtk::Widget::NONE);
        return true;
    }
    overlay.remove_overlay(child);
    true
}

fn unpane(paned: &gtk::Paned, child: &gtk::Widget) -> bool {
    if paned.start_child().is_some_and(|held| held.eq(child)) {
        paned.set_start_child(gtk::Widget::NONE);
        return true;
    }
    paned.set_end_child(gtk::Widget::NONE);
    true
}

/// Layout facts that GTK keeps nowhere until a widget is placed.
///
/// A producer describes a fresh node — its column count, its span, its
/// alignment — and only then inserts it, so every one of these arrives before
/// the widget has a parent to interpret it. GTK4 offers no per-widget user data
/// reachable without `unsafe`, so the adapter keeps them beside the widget.
#[derive(Clone, Copy, Debug)]
struct Slot {
    /// Columns this widget lays its children out in, when it is a grid.
    columns: u16,
    /// Columns and rows this widget occupies in the grid holding it.
    span: u16,
    rows: u16,
    /// Placement along the parent's main and cross axes, resolved once the
    /// parent — and therefore the meaning of "main" — is known.
    main: Option<Align>,
    cross: Option<Align>,
}

impl Default for Slot {
    fn default() -> Self {
        // One column is the only column count that needs no description: it is
        // what a grid with nothing said about it must do, and it never places a
        // child somewhere the producer did not ask for.
        Self {
            columns: 1,
            span: 1,
            rows: 1,
            main: None,
            cross: None,
        }
    }
}

thread_local! {
    /// Weak, so an entry never keeps a widget alive; pruned on every visit, so
    /// a surface that churns widgets does not grow this without bound.
    static SLOTS: RefCell<Vec<(gtk::glib::WeakRef<gtk::Widget>, Slot)>> = const { RefCell::new(Vec::new()) };
}

fn slot(widget: &gtk::Widget) -> Slot {
    SLOTS.with_borrow(|slots| {
        slots
            .iter()
            .find(|(held, _)| held.upgrade().is_some_and(|alive| alive.eq(widget)))
            .map_or_else(Slot::default, |(_, slot)| *slot)
    })
}

/// Records a layout fact, reporting whether the widget was unknown until now.
fn amend(widget: &gtk::Widget, change: impl FnOnce(&mut Slot)) -> bool {
    SLOTS.with_borrow_mut(|slots| {
        slots.retain(|(held, _)| held.upgrade().is_some());
        let found = slots
            .iter_mut()
            .find(|(held, _)| held.upgrade().is_some_and(|alive| alive.eq(widget)));
        if let Some((_, slot)) = found {
            change(slot);
            return false;
        }
        let mut slot = Slot::default();
        change(&mut slot);
        slots.push((widget.downgrade(), slot));
        true
    })
}

/// Applies a grid's column count and re-flows what it already holds.
pub(crate) fn columns(widget: &gtk::Widget, value: &PropValue) {
    let Some(grid) = widget.downcast_ref::<gtk::Grid>() else {
        return;
    };
    amend(widget, |slot| slot.columns = value.as_count().unwrap_or(1));
    arrange(grid, &offspring(grid));
}

/// Applies a child's column or row span and re-flows the grid holding it.
pub(crate) fn span(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    let cells = value.as_count().unwrap_or(1);
    amend(widget, |slot| match prop {
        Prop::Span => slot.span = cells,
        _ => slot.rows = cells,
    });
    let Some(grid) = widget.parent().and_then(|parent| parent.downcast::<gtk::Grid>().ok()) else {
        return;
    };
    arrange(&grid, &offspring(&grid));
}

fn offspring(grid: &gtk::Grid) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut cursor = grid.first_child();
    while let Some(child) = cursor {
        cursor = child.next_sibling();
        found.push(child);
    }
    found
}

/// Places a new child among its siblings, then re-flows the whole grid.
///
/// The grid is attached to first because a widget only has a layout child — the
/// object carrying its cell — once the container holds it.
fn seat(grid: &gtk::Grid, child: &gtk::Widget, index: usize) {
    let mut order = offspring(grid);
    grid.attach(child, 0, 0, 1, 1);
    order.insert(index.min(order.len()), child.clone());
    arrange(grid, &order);
}

/// Flows children into cells: left to right across the declared columns, onto a
/// new row when the next child does not fit, and never onto a cell a spanning
/// child above has already claimed.
fn arrange(grid: &gtk::Grid, order: &[gtk::Widget]) {
    let columns = i32::from(slot(grid.upcast_ref()).columns.max(1));
    let mut taken: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut cursor = (0, 0);
    for child in order {
        let held = slot(child);
        let span = i32::from(held.span).min(columns);
        let rows = i32::from(held.rows);
        cursor = vacancy(&taken, columns, (span, rows), cursor);
        claim(&mut taken, cursor, (span, rows));
        place(grid, child, cursor, (span, rows));
        cursor = (cursor.0 + span, cursor.1);
    }
}

/// The first cell at or after `from` where a child of this size fits.
fn vacancy(taken: &BTreeSet<(i32, i32)>, columns: i32, size: (i32, i32), from: (i32, i32)) -> (i32, i32) {
    let (span, _) = size;
    let mut cell = from;
    while cell.0 + span > columns || !vacant(taken, cell, size) {
        cell = if cell.0 + span > columns {
            (0, cell.1 + 1)
        } else {
            (cell.0 + 1, cell.1)
        };
    }
    cell
}

fn vacant(taken: &BTreeSet<(i32, i32)>, cell: (i32, i32), size: (i32, i32)) -> bool {
    let cells = (0..size.0).flat_map(|column| (0..size.1).map(move |row| (column, row)));
    !cells
        .into_iter()
        .any(|(column, row)| taken.contains(&(cell.0 + column, cell.1 + row)))
}

fn claim(taken: &mut BTreeSet<(i32, i32)>, cell: (i32, i32), size: (i32, i32)) {
    let cells = (0..size.0).flat_map(|column| (0..size.1).map(move |row| (column, row)));
    for (column, row) in cells {
        taken.insert((cell.0 + column, cell.1 + row));
    }
}

/// Moves an attached child to a cell through the grid's own layout child, which
/// is where GTK keeps the placement `gtk::Grid::query_child` reports.
fn place(grid: &gtk::Grid, child: &gtk::Widget, cell: (i32, i32), size: (i32, i32)) {
    let Some(layout) = grid
        .layout_manager()
        .and_then(|manager| manager.downcast::<gtk::GridLayout>().ok())
    else {
        return;
    };
    let Ok(seat) = layout.layout_child(child).downcast::<gtk::GridLayoutChild>() else {
        return;
    };
    seat.set_column(cell.0);
    seat.set_row(cell.1);
    seat.set_column_span(size.0);
    seat.set_row_span(size.1);
}

/// Makes a row or column wrap its children onto further lines, or stop doing so.
///
/// The wrapping layout replaces the box's own, rather than the container being
/// built as a `gtk::FlowBox`: properties arrive after the node is created, so
/// the widget class is already chosen by the time a producer says "wrap".
pub(crate) fn wrap(widget: &gtk::Widget, axis: Orientation, wrapping: bool) {
    if widget.downcast_ref::<gtk::Box>().is_none() {
        return;
    }
    if !wrapping {
        widget.set_layout_manager(Some(gtk::BoxLayout::new(direction(axis))));
        return;
    }
    let flow = Flow::new(direction(axis));
    flow.set_spacing(spacing(widget));
    widget.set_layout_manager(Some(flow));
}

/// The gap a container was already given, so turning wrapping on does not
/// silently reset spacing the producer described earlier.
fn spacing(widget: &gtk::Widget) -> i32 {
    let Some(manager) = widget.layout_manager() else {
        return 0;
    };
    if let Some(container) = manager.downcast_ref::<gtk::BoxLayout>() {
        return i32::try_from(container.spacing()).unwrap_or(0);
    }
    manager.downcast_ref::<Flow>().map_or(0, Flow::spacing)
}

/// Applies a gap to whichever layout the container is running.
pub(crate) fn gap(widget: &gtk::Widget, pixels: i32) -> bool {
    let Some(manager) = widget.layout_manager() else {
        return false;
    };
    if let Some(flow) = manager.downcast_ref::<Flow>() {
        flow.set_spacing(pixels);
        return true;
    }
    false
}

/// Applies an orientation to a wrapping container, which owns its own axis.
pub(crate) fn direct(widget: &gtk::Widget, axis: Orientation) -> bool {
    let Some(flow) = widget
        .layout_manager()
        .and_then(|manager| manager.downcast::<Flow>().ok())
    else {
        return false;
    };
    flow.set_direction(direction(axis));
    true
}

fn direction(axis: Orientation) -> gtk::Orientation {
    match axis {
        Orientation::Horizontal => gtk::Orientation::Horizontal,
        Orientation::Vertical => gtk::Orientation::Vertical,
    }
}

/// Records main- or cross-axis placement and applies it.
///
/// Which GTK alignment a request means depends on the container: the main axis
/// of a row is horizontal and of a column vertical. A child is described before
/// it is inserted, so the answer is unknown here — the widget is watched for
/// gaining a parent, and settled again then.
pub(crate) fn alignment(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    let placement = match value {
        PropValue::Align(align) => Some(*align),
        _ => None,
    };
    let fresh = amend(widget, |slot| match prop {
        Prop::Align => slot.main = placement,
        _ => slot.cross = placement,
    });
    if fresh {
        widget.connect_parent_notify(settle);
    }
    settle(widget);
}

/// Resolves stored placement against the container the widget now sits in.
pub(crate) fn settle(widget: &gtk::Widget) {
    let held = slot(widget);
    let (main, cross) = (held.main, held.cross);
    if main.is_none() && cross.is_none() {
        return;
    }
    let vertical = widget
        .parent()
        .is_some_and(|parent| main_axis(&parent) == gtk::Orientation::Vertical);
    let (horizontal_intent, vertical_intent) = if vertical { (cross, main) } else { (main, cross) };
    if let Some(align) = horizontal_intent {
        widget.set_halign(placement(align));
    }
    if let Some(align) = vertical_intent {
        widget.set_valign(placement(align));
    }
}

/// The axis a container advances its children along. A grid advances across
/// columns, and anything that is not a line of children has no main axis of its
/// own — horizontal is the reading order and the harmless answer.
fn main_axis(parent: &gtk::Widget) -> gtk::Orientation {
    if let Some(flow) = parent
        .layout_manager()
        .and_then(|manager| manager.downcast::<Flow>().ok())
    {
        return flow.direction();
    }
    match parent.downcast_ref::<gtk::Box>() {
        Some(container) => container.orientation(),
        None => gtk::Orientation::Horizontal,
    }
}

fn placement(align: Align) -> gtk::Align {
    match align {
        Align::Start => gtk::Align::Start,
        Align::Center => gtk::Align::Center,
        Align::End => gtk::Align::End,
        Align::Stretch => gtk::Align::Fill,
    }
}
