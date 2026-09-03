//! Where the toolkit actually puts things.
//!
//! Presence tests — "the child reached the widget tree" — cannot tell a grid
//! that places its fourth child on the second row from one that stacks every
//! child in the same cell. These scenarios read the placement back out of GTK:
//! the cell a grid gave a child, the margins a padded widget carries, the
//! coordinates a wrapping row allocated, and the axis an alignment landed on.
//!
//! GTK initializes once per process, so this is one test running its scenarios
//! in sequence rather than a test per scenario.

use gtk::prelude::*;
use hl_gui::{Align, Bounds, Edges, Length, NodeId, Orientation, Prop, PropValue, Tag, Tree};
use hl_gui_gtk::Surface;

/// Pixels a child asks for on its main axis, so wrapping is arithmetic rather
/// than a guess about font metrics.
const CHILD: i32 = 48;

/// One producer, one tree, one rendered surface.
struct Stage {
    producer: hl_gui::Surface,
    tree: Tree,
    surface: Surface,
}

impl Stage {
    fn new() -> Self {
        Self {
            producer: hl_gui::Surface::new(),
            tree: Tree::new(),
            surface: Surface::new(),
        }
    }

    /// Renders everything described since the last call.
    fn draw(&mut self) {
        let frame = self.producer.frame();
        self.tree
            .apply(&frame, &mut self.surface)
            .expect("the adapter applies every patch");
    }

    /// A child of a fixed main-axis size, described but not yet attached.
    fn block(&mut self, label: &str) -> NodeId {
        let node = self.producer.create(Tag::Text);
        self.producer.set(node, Prop::Label, PropValue::text(label));
        let size = Bounds::between(Length::Step(12), Length::Step(12));
        self.producer.set(node, Prop::Width, PropValue::Bounds(size));
        self.producer.set(node, Prop::Height, PropValue::Bounds(size));
        node
    }

    /// The first widget carrying a tag's style class, which is the adapter's
    /// own public naming.
    fn tagged(&self, tag: Tag) -> gtk::Widget {
        let class = format!("hl-{}", tag.as_str().to_ascii_lowercase());
        let mut found = vec![self.surface.widget().clone().upcast::<gtk::Widget>()];
        let mut index = 0;
        while index < found.len() {
            found.extend(offspring(&found[index]));
            index += 1;
        }
        found
            .into_iter()
            .find(|widget| widget.has_css_class(&class))
            .unwrap_or_else(|| panic!("no {} was rendered", tag.as_str()))
    }

    /// Lays the whole surface out at a fixed size, so allocations are the ones
    /// this width implies rather than whatever a window happened to negotiate.
    fn allocate(&self, width: i32, height: i32) {
        let root = self.surface.widget().clone().upcast::<gtk::Widget>();
        root.measure(gtk::Orientation::Horizontal, -1);
        root.measure(gtk::Orientation::Vertical, width);
        root.allocate(width, height, -1, None);
    }
}

fn offspring(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut cursor = widget.first_child();
    while let Some(child) = cursor {
        cursor = child.next_sibling();
        found.push(child);
    }
    found
}

/// The cells a grid gave each of its children, in attachment order.
fn cells(grid: &gtk::Grid) -> Vec<(i32, i32, i32, i32)> {
    offspring(grid.upcast_ref())
        .iter()
        .map(|child| grid.query_child(child))
        .collect()
}

#[test]
fn geometry_is_what_the_description_asked_for() {
    assert!(
        gtk::init().is_ok(),
        "these scenarios need a display; run them against broadwayd"
    );
    a_grid_flows_into_its_declared_columns();
    a_spanning_child_occupies_the_cells_it_claims();
    a_tall_child_pushes_the_next_row_around_it();
    a_removed_child_closes_the_hole_it_left();
    a_wrapping_row_moves_a_child_onto_a_second_line();
    a_wrapping_row_shares_spare_width_between_growing_children();
    every_wrapped_line_distributes_its_own_spare_width();
    a_wrapping_column_shares_spare_height_between_growing_children();
    a_wrapping_row_follows_right_to_left_order();
    padding_lands_on_the_side_it_names();
    alignment_follows_the_axis_of_its_container();
    a_size_range_becomes_a_floor_the_toolkit_honours();
    a_scrolled_pane_shares_narrow_and_wide_host_width();
}

/// Storybook's catalogue and inspector are scrolling panes. They must share the
/// host width instead of asking GTK for a fixed character width that scrolling
/// viewports cannot express.
fn a_scrolled_pane_shares_narrow_and_wide_host_width() {
    let mut stage = Stage::new();
    let scroll = stage.producer.create(Tag::Scroll);
    stage.producer.set(scroll, Prop::Width, PropValue::Length(Length::Fill));
    let label = stage.producer.create(Tag::Text);
    stage.producer.set(
        label,
        Prop::Label,
        PropValue::text("a deliberately very long inspector value that must not establish the pane width"),
    );
    stage.producer.append(scroll, label);
    stage.producer.append(NodeId::ROOT, scroll);
    stage.draw();

    let widget = stage.tagged(Tag::Scroll);
    let (minimum, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    assert!(widget.hexpands(), "the pane does not share available width");
    assert!(minimum <= 300, "scrolling pane minimum overflowed: {minimum}px");
    assert!(
        natural <= 300,
        "scrolling pane natural width ignored its ceiling: {natural}px"
    );
}

/// Four children in three columns: the fourth starts the second row. This is
/// the placement the adapter used to get wrong by reading a pixel gap as a
/// column count, which a presence test could not see.
fn a_grid_flows_into_its_declared_columns() {
    let mut stage = Stage::new();
    let grid = stage.producer.create(Tag::Grid);
    stage.producer.set(grid, Prop::Columns, PropValue::Integer(3));
    stage.producer.append(NodeId::ROOT, grid);
    for index in 0..4 {
        let child = stage.block(&format!("cell {index}"));
        stage.producer.append(grid, child);
    }
    stage.draw();
    let widget = stage.tagged(Tag::Grid);
    let grid = widget.downcast_ref::<gtk::Grid>().expect("a grid is a gtk::Grid");
    assert_eq!(
        cells(grid),
        vec![(0, 0, 1, 1), (1, 0, 1, 1), (2, 0, 1, 1), (0, 1, 1, 1)],
        "the fourth of three columns starts a new row"
    );
}

/// A child spanning two of three columns leaves one column beside it, and the
/// next child after that starts the following row.
fn a_spanning_child_occupies_the_cells_it_claims() {
    let mut stage = Stage::new();
    let grid = stage.producer.create(Tag::Grid);
    stage.producer.set(grid, Prop::Columns, PropValue::Integer(3));
    stage.producer.append(NodeId::ROOT, grid);
    let wide = stage.block("wide");
    stage.producer.set(wide, Prop::Span, PropValue::Integer(2));
    stage.producer.append(grid, wide);
    for index in 0..2 {
        let child = stage.block(&format!("cell {index}"));
        stage.producer.append(grid, child);
    }
    stage.draw();
    let widget = stage.tagged(Tag::Grid);
    let grid = widget.downcast_ref::<gtk::Grid>().expect("a grid is a gtk::Grid");
    assert_eq!(
        cells(grid),
        vec![(0, 0, 2, 1), (2, 0, 1, 1), (0, 1, 1, 1)],
        "a span occupies its cells and displaces what follows"
    );
}

/// A child two rows tall keeps its column on the row beneath it, so the flow
/// steps around it instead of placing a second child in the same cell.
fn a_tall_child_pushes_the_next_row_around_it() {
    let mut stage = Stage::new();
    let grid = stage.producer.create(Tag::Grid);
    stage.producer.set(grid, Prop::Columns, PropValue::Integer(3));
    stage.producer.append(NodeId::ROOT, grid);
    let tall = stage.block("tall");
    stage.producer.set(tall, Prop::RowSpan, PropValue::Integer(2));
    stage.producer.append(grid, tall);
    for index in 0..3 {
        let child = stage.block(&format!("cell {index}"));
        stage.producer.append(grid, child);
    }
    stage.draw();
    let widget = stage.tagged(Tag::Grid);
    let grid = widget.downcast_ref::<gtk::Grid>().expect("a grid is a gtk::Grid");
    assert_eq!(
        cells(grid),
        vec![(0, 0, 1, 2), (1, 0, 1, 1), (2, 0, 1, 1), (1, 1, 1, 1)],
        "the second row starts beside the tall child, not underneath it"
    );
}

/// Removing a child re-flows the rest, because a grid describes an arrangement
/// and not a set of fixed addresses.
fn a_removed_child_closes_the_hole_it_left() {
    let mut stage = Stage::new();
    let grid = stage.producer.create(Tag::Grid);
    stage.producer.set(grid, Prop::Columns, PropValue::Integer(2));
    stage.producer.append(NodeId::ROOT, grid);
    let mut children = Vec::new();
    for index in 0..4 {
        let child = stage.block(&format!("cell {index}"));
        stage.producer.append(grid, child);
        children.push(child);
    }
    stage.draw();
    stage.producer.remove(children[0]);
    stage.draw();
    let widget = stage.tagged(Tag::Grid);
    let grid = widget.downcast_ref::<gtk::Grid>().expect("a grid is a gtk::Grid");
    assert_eq!(
        cells(grid),
        vec![(0, 0, 1, 1), (1, 0, 1, 1), (0, 1, 1, 1)],
        "the survivors move up one cell"
    );
}

/// Two 48px children fit a 100px row; the third has to go somewhere, and a
/// wrapping row puts it on the next line rather than off the end.
fn a_wrapping_row_moves_a_child_onto_a_second_line() {
    let mut stage = Stage::new();
    let row = stage.producer.create(Tag::Row);
    stage.producer.set(row, Prop::Wrap, PropValue::Flag(true));
    stage.producer.append(NodeId::ROOT, row);
    let mut children = Vec::new();
    for index in 0..3 {
        let child = stage.block(&format!("cell {index}"));
        stage.producer.append(row, child);
        children.push(child);
    }
    stage.draw();
    stage.allocate(100, 400);
    let widget = stage.tagged(Tag::Row);
    let placed: Vec<(i32, i32)> = offspring(&widget)
        .iter()
        .map(|child| (child.allocation().x(), child.allocation().y()))
        .collect();
    assert_eq!(placed[0], (0, 0));
    assert_eq!(placed[1], (CHILD, 0), "the second child still fits the line");
    assert_eq!(placed[2].0, 0, "the third child starts the next line");
    assert!(
        placed[2].1 >= CHILD,
        "the third child sits below the first line, at {}",
        placed[2].1
    );
    // Width decides height here, which is the whole claim: the same row is two
    // lines tall when it is narrow and one line tall when it is not.
    let (narrow, _, _, _) = widget.measure(gtk::Orientation::Vertical, 100);
    let (wide, _, _, _) = widget.measure(gtk::Orientation::Vertical, 400);
    assert_eq!(narrow, CHILD * 2, "three children in two lines");
    assert_eq!(wide, CHILD, "the same three children on one line");
}

/// A wrapping row replaces GTK's box layout, but `grow` must retain the same
/// meaning when all children fit on one line.
fn a_wrapping_row_shares_spare_width_between_growing_children() {
    let mut stage = Stage::new();
    let row = stage.producer.create(Tag::Row);
    stage.producer.set(row, Prop::Wrap, PropValue::Flag(true));
    stage.producer.append(NodeId::ROOT, row);
    for _ in 0..2 {
        let pane = stage.producer.create(Tag::Scroll);
        stage.producer.set(pane, Prop::Width, PropValue::Length(Length::Fill));
        stage.producer.append(row, pane);
    }
    stage.draw();
    stage.allocate(600, 200);

    let panes = offspring(&stage.tagged(Tag::Row));
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].width(), 300);
    assert_eq!(panes[1].width(), 300);
}

fn every_wrapped_line_distributes_its_own_spare_width() {
    let mut stage = Stage::new();
    let row = stage.producer.create(Tag::Row);
    stage.producer.set(row, Prop::Wrap, PropValue::Flag(true));
    stage.producer.append(NodeId::ROOT, row);
    for _ in 0..3 {
        let child = stage.producer.create(Tag::Scroll);
        stage.producer.append(row, child);
    }
    stage.draw();
    for child in offspring(&stage.tagged(Tag::Row)) {
        child.set_size_request(CHILD, CHILD);
    }
    stage.allocate(100, 200);

    let children = offspring(&stage.tagged(Tag::Row));
    assert_eq!((children[0].width(), children[1].width()), (50, 50));
    assert_eq!(children[2].width(), 100, "the single child on line two owns its line");
}

fn a_wrapping_column_shares_spare_height_between_growing_children() {
    let mut stage = Stage::new();
    let column = stage.producer.create(Tag::Column);
    stage.producer.set(column, Prop::Wrap, PropValue::Flag(true));
    stage.producer.append(NodeId::ROOT, column);
    for _ in 0..2 {
        let pane = stage.producer.create(Tag::Scroll);
        stage.producer.set(pane, Prop::Height, PropValue::Length(Length::Fill));
        stage.producer.append(column, pane);
    }
    stage.draw();
    stage.allocate(200, 600);

    let panes = offspring(&stage.tagged(Tag::Column));
    assert_eq!(panes[0].height(), 300);
    assert_eq!(panes[1].height(), 300);
}

fn a_wrapping_row_follows_right_to_left_order() {
    let mut stage = Stage::new();
    let row = stage.producer.create(Tag::Row);
    stage.producer.set(row, Prop::Wrap, PropValue::Flag(true));
    stage.producer.append(NodeId::ROOT, row);
    for index in 0..2 {
        let child = stage.block(&format!("cell {index}"));
        stage.producer.append(row, child);
    }
    stage.draw();
    let widget = stage.tagged(Tag::Row);
    widget.set_direction(gtk::TextDirection::Rtl);
    stage.allocate(100, 100);

    let children = offspring(&widget);
    assert_eq!(children[0].allocation().x(), 52, "the first child starts at the right edge");
    assert_eq!(children[1].allocation().x(), 4, "the second child follows toward the left");
}

/// One property, four sides, each landing where it was named.
fn padding_lands_on_the_side_it_names() {
    let mut stage = Stage::new();
    let column = stage.producer.create(Tag::Column);
    let edges = Edges::sides(Length::Step(1), Length::Step(2), Length::Step(3), Length::Step(4));
    stage.producer.set(column, Prop::Pad, PropValue::Edges(edges));
    stage.producer.append(NodeId::ROOT, column);
    stage.draw();
    let widget = stage.tagged(Tag::Column);
    assert_eq!(widget.margin_top(), 4);
    assert_eq!(widget.margin_end(), 8);
    assert_eq!(widget.margin_bottom(), 12);
    assert_eq!(widget.margin_start(), 16);

    // A plain length still means every side, so the older description holds.
    let mut plain = Stage::new();
    let card = plain.producer.create(Tag::Column);
    plain.producer.set(card, Prop::Pad, PropValue::Length(Length::Step(2)));
    plain.producer.append(NodeId::ROOT, card);
    plain.draw();
    let padded = plain.tagged(Tag::Column);
    assert_eq!(
        (
            padded.margin_top(),
            padded.margin_end(),
            padded.margin_bottom(),
            padded.margin_start()
        ),
        (8, 8, 8, 8)
    );
}

/// The main axis of a row is horizontal and of a column vertical, so the same
/// described alignment has to reach different GTK properties.
fn alignment_follows_the_axis_of_its_container() {
    let mut stage = Stage::new();
    let column = stage.producer.create(Tag::Column);
    stage
        .producer
        .set(column, Prop::Orientation, PropValue::Orientation(Orientation::Vertical));
    stage.producer.append(NodeId::ROOT, column);
    let row = stage.producer.create(Tag::Row);
    stage.producer.append(NodeId::ROOT, row);
    // Both children are described before they are inserted, which is the order
    // a producer works in and the order that hides the container from the
    // adapter until afterwards.
    let stacked = stage.block("stacked");
    stage.producer.set(stacked, Prop::Align, PropValue::Align(Align::End));
    stage
        .producer
        .set(stacked, Prop::Justify, PropValue::Align(Align::Center));
    stage.producer.append(column, stacked);
    let inline = stage.block("inline");
    stage.producer.set(inline, Prop::Align, PropValue::Align(Align::End));
    stage
        .producer
        .set(inline, Prop::Justify, PropValue::Align(Align::Center));
    stage.producer.append(row, inline);
    stage.draw();

    let held = offspring(&stage.tagged(Tag::Column));
    let placed = held.first().expect("the column holds its child");
    assert_eq!(placed.valign(), gtk::Align::End, "a column advances downwards");
    assert_eq!(placed.halign(), gtk::Align::Center, "and shares its width across");
    let inline = offspring(&stage.tagged(Tag::Row));
    let placed = inline.first().expect("the row holds its child");
    assert_eq!(placed.halign(), gtk::Align::End, "a row advances rightwards");
    assert_eq!(placed.valign(), gtk::Align::Center, "and shares its height across");
}

/// A floor is a size request, which is a promise GTK keeps for any widget.
fn a_size_range_becomes_a_floor_the_toolkit_honours() {
    let mut stage = Stage::new();
    let text = stage.producer.create(Tag::Text);
    stage.producer.set(text, Prop::Label, PropValue::text("x"));
    let bounds = Bounds::at_least(Length::Step(12));
    stage.producer.set(text, Prop::Width, PropValue::Bounds(bounds));
    stage.producer.append(NodeId::ROOT, text);
    stage.draw();
    let widget = stage.tagged(Tag::Text);
    let (minimum, _, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    assert_eq!(widget.width_request(), CHILD);
    assert!(minimum >= CHILD, "a single character still measures {CHILD} wide");
}
