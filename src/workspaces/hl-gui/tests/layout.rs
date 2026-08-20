//! The layout vocabulary, judged without a toolkit.
//!
//! Everything here is a rule an adapter is entitled to rely on: what a padding
//! value means on each side, what a size range means when its ends contradict
//! each other, and what a cell count means when a producer describes nonsense.
//! An adapter that has to defend against zero-column children is an adapter
//! that will place them somewhere no one asked for, so the defence lives here.

use hl_gui::{Bounds, Edges, Length, NodeId, Patch, Prop, PropValue, RowWindow, Surface, Theme, Tree};

/// A renderer that keeps nothing, so a round-trip is judged against the tree
/// the library retained rather than against anything a toolkit inferred.
#[derive(Default)]
struct Sink {
    applied: usize,
}

impl hl_gui::Renderer for Sink {
    type Error = ();

    fn patch(&mut self, _patch: &Patch, _tree: &Tree) -> Result<(), Self::Error> {
        self.applied += 1;
        Ok(())
    }

    fn commit(&mut self, _sequence: u64) -> Result<(), Self::Error> {
        Ok(())
    }

    fn rows(&mut self, _window: &RowWindow) -> Result<(), Self::Error> {
        Ok(())
    }

    fn theme(&mut self, _theme: &Theme) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Applies one frame of properties to a fresh grid node and returns the tree.
fn retained(props: &[(Prop, PropValue)]) -> (Tree, NodeId) {
    let mut surface = Surface::new();
    let mut tree = Tree::new();
    let mut sink = Sink::default();
    let node = surface.create(hl_gui::Tag::Grid);
    for (prop, value) in props {
        surface.set(node, *prop, value.clone());
    }
    surface.append(NodeId::ROOT, node);
    let frame = surface.frame();
    tree.apply(&frame, &mut sink).expect("the sink accepts every patch");
    (tree, node)
}

#[test]
fn layout_properties_survive_a_frame_unchanged() {
    let edges = Edges::sides(Length::Step(1), Length::Step(2), Length::Step(3), Length::Step(4));
    let bounds = Bounds::between(Length::Step(4), Length::Step(10));
    let (tree, node) = retained(&[
        (Prop::Columns, PropValue::Integer(3)),
        (Prop::Span, PropValue::Integer(2)),
        (Prop::Pad, PropValue::Edges(edges)),
        (Prop::Width, PropValue::Bounds(bounds)),
    ]);
    let held = tree.node(node).expect("the node was created");
    assert_eq!(held.prop(Prop::Columns).and_then(PropValue::as_count), Some(3));
    assert_eq!(held.prop(Prop::Span).and_then(PropValue::as_count), Some(2));
    assert_eq!(held.prop(Prop::Pad).and_then(PropValue::as_edges), Some(edges));
    assert_eq!(held.prop(Prop::Width).and_then(PropValue::as_bounds), Some(bounds));
}

#[test]
fn a_span_is_never_zero() {
    assert_eq!(PropValue::Integer(0).as_count(), Some(1), "a child occupies a cell");
    assert_eq!(PropValue::Integer(-4).as_count(), Some(1));
    assert_eq!(PropValue::Number(0.5).as_count(), Some(1));
    assert_eq!(PropValue::Number(f64::NAN).as_count(), Some(1));
    assert_eq!(PropValue::Number(f64::NEG_INFINITY).as_count(), Some(1));
    assert_eq!(PropValue::text("two").as_count(), None, "a word is not a count");
}

#[test]
fn a_cell_count_saturates_instead_of_wrapping_around() {
    assert_eq!(PropValue::Integer(3).as_count(), Some(3));
    assert_eq!(
        PropValue::Number(2.9).as_count(),
        Some(2),
        "a partial column is not a column"
    );
    assert_eq!(PropValue::Integer(1 << 40).as_count(), Some(u16::MAX));
    assert_eq!(PropValue::Number(f64::INFINITY).as_count(), Some(1));
}

#[test]
fn one_padding_value_describes_one_two_or_four_sides() {
    let all = Edges::all(Length::Step(2));
    assert_eq!(
        (all.top, all.end, all.bottom, all.start),
        (all.top, all.top, all.top, all.top)
    );
    let pair = Edges::symmetric(Length::Step(1), Length::Step(3));
    assert_eq!(pair.top, Length::Step(1));
    assert_eq!(pair.bottom, Length::Step(1));
    assert_eq!(pair.start, Length::Step(3));
    assert_eq!(pair.end, Length::Step(3));
    let sides = Edges::sides(Length::Step(1), Length::Step(2), Length::Step(3), Length::Step(4));
    assert_eq!(sides.top, Length::Step(1));
    assert_eq!(sides.end, Length::Step(2));
    assert_eq!(sides.bottom, Length::Step(3));
    assert_eq!(sides.start, Length::Step(4));
    assert_eq!(Edges::none(), Edges::default(), "clearing padding leaves none");
}

#[test]
fn a_plain_length_still_pads_every_side() {
    let read = PropValue::Length(Length::Step(2))
        .as_edges()
        .expect("a length is padding");
    assert_eq!(
        read,
        Edges::all(Length::Step(2)),
        "the simple description keeps working"
    );
}

#[test]
fn a_ceiling_below_its_floor_is_raised_to_it() {
    let bounds = Bounds::between(Length::Step(6), Length::Step(2));
    assert_eq!(bounds.minimum, Some(Length::Step(6)));
    assert_eq!(
        bounds.maximum,
        Some(Length::Step(6)),
        "an impossible range fixes the size"
    );
    let ordered = Bounds::between(Length::Step(2), Length::Step(6));
    assert_eq!(ordered.maximum, Some(Length::Step(6)));
    assert_eq!(Bounds::at_least(Length::Step(3)).maximum, None);
    assert_eq!(Bounds::at_most(Length::Step(3)).minimum, None);
}

#[test]
fn a_step_beyond_the_scale_clamps_rather_than_growing_forever() {
    // The spacing scale is generated, so a producer asking for step 200 must
    // land on a real class instead of an unstyled margin.
    assert_eq!(Length::Step(200).pixels(), Length::Step(Length::MAXIMUM_STEP).pixels());
    let bounds = Bounds::between(Length::Step(200), Length::Step(20));
    assert_eq!(
        bounds.minimum.and_then(Length::pixels),
        bounds.maximum.and_then(Length::pixels),
        "two clamped lengths compare as the same size"
    );
}
