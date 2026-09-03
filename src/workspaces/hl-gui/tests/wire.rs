//! Serialization of the interface model.
//!
//! An out-of-process producer describes an interface by sending patches, so
//! every component, property, and style value has to survive the round trip
//! unchanged. These run only with the optional `wire` feature; without it the
//! library stays dependency-free.
//!
//! # Which invocations actually run these
//!
//! `wire` is not a default feature, so a per-package run compiles the eight tests
//! below out and reports `running 0 tests ... ok`. That is an empty target, not a
//! pass, and the two are indistinguishable because libtest shows captured output
//! only for tests that fail. Measured on `x86_64` Linux at 641d3f580:
//!
//! ```text
//! cargo test -p hl-gui --test wire ................... 0 tests
//! cargo test -p hl-gui --all-features --test wire .... 8 tests
//! cargo test -p hl-gui -p hl-extension --test wire ... 8 tests
//! ```
//!
//! The third reading is why these are not uncovered. `hl-extension` and the
//! extension application both depend on `hl-gui` with `features = ["wire"]`, so
//! Cargo unifies `wire` on for every package selection that also builds one of
//! them -- including the `cargo test --workspace --all-targets` that the
//! `nix flake check` verification derivation runs. The eight tests run there.
//!
//! That coverage is real but incidental: it holds only while some other workspace
//! member happens to want `wire`, and nothing asserts that it does. The feature
//! itself is deliberately optional -- keeping the library dependency-free for an
//! embedding application is the manifest's stated reason for it -- so the honest
//! disposition is not to force `wire` on, but to stop the compiled-out case from
//! impersonating a passing one. That is what the notice below is for.

/// Says out loud that this target compiled to zero round-trip tests.
///
/// libtest captures `print!` and `eprint!` and reveals the capture only for tests
/// that fail, so a notice written through those macros is discarded on the green
/// path this test always takes. The standard-error handle is written directly
/// instead, which bypasses the capture for the same reason the rest of the
/// repository reaches for `libc::write(2, ...)`; `hl-gui` carries no `libc`
/// dependency and does not need one to do it.
#[cfg(not(feature = "wire"))]
#[test]
fn the_wire_round_trips_are_compiled_out_without_the_wire_feature() {
    use std::io::Write as _;
    let _ = std::io::stderr().write_all(
        b"SKIP: hl-gui/tests/wire.rs compiled to zero round-trip tests because the `wire` \
feature is off. Run `cargo test -p hl-gui --all-features`, or any package selection that \
also builds hl-extension, to exercise the eight tests this file holds.\n",
    );
}

#[cfg(feature = "wire")]
use hl_gui::{
    Align, Cell, Column, Density, EventId, Frame, Handler, Length, NodeId, Orientation, Patch, Prop, PropValue,
    RequestId, Row, RowRange, RowRequest, RowWindow, Scale, SourceId, SourceMutation, Surface, Tag, Theme, Tone,
    Trigger, Version,
};

#[cfg(feature = "wire")]
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let encoded = serde_json::to_string(value).expect("serialized");
    serde_json::from_str(&encoded).expect("deserialized")
}

#[cfg(feature = "wire")]
#[test]
fn every_component_tag_survives_the_wire() {
    for tag in Tag::ALL {
        let patch = Patch::Create {
            id: NodeId::new(1),
            tag: *tag,
        };
        assert_eq!(round_trip(&patch), patch, "{} did not round trip", tag.as_str());
    }
}

#[cfg(feature = "wire")]
#[test]
fn every_property_value_shape_survives_the_wire() {
    let values = [
        PropValue::text("label"),
        PropValue::Number(1.5),
        PropValue::Integer(-7),
        PropValue::Flag(true),
        PropValue::Token(hl_gui::Token::Accent),
        PropValue::Length(Length::Step(3)),
        PropValue::Length(Length::Chars(12)),
        PropValue::Length(Length::Fill),
        PropValue::Length(Length::Content),
        PropValue::Variant(hl_gui::Variant::Outline),
        PropValue::Tone(Tone::Danger),
        PropValue::Scale(Scale::Title),
        PropValue::Align(Align::Center),
        PropValue::Orientation(Orientation::Vertical),
        PropValue::Choices(vec![hl_gui::Choice::new("a", "Always")]),
        PropValue::Schema(vec![Column::new("name", "Name").width(Length::Fill).sortable()]),
        PropValue::Source(SourceId::new(9)),
        PropValue::Nothing,
    ];
    for value in values {
        assert_eq!(round_trip(&value), value, "{value:?} did not round trip");
    }
}

#[cfg(feature = "wire")]
#[test]
fn every_property_name_survives_the_wire() {
    for prop in [
        Prop::Label,
        Prop::Value,
        Prop::Placeholder,
        Prop::Enabled,
        Prop::Checked,
        Prop::Variant,
        Prop::Tone,
        Prop::Scale,
        Prop::Gap,
        Prop::Pad,
        Prop::Width,
        Prop::Height,
        Prop::Align,
        Prop::Schema,
        Prop::Source,
        Prop::Choices,
        Prop::Fraction,
    ] {
        let patch = Patch::ClearProp {
            id: NodeId::new(1),
            prop,
        };
        assert_eq!(round_trip(&patch), patch, "{prop:?} did not round trip");
    }
}

#[cfg(feature = "wire")]
#[test]
fn every_mutation_shape_survives_the_wire() {
    let patches = [
        Patch::Create {
            id: NodeId::new(1),
            tag: Tag::Card,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: NodeId::new(1),
            before: None,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: NodeId::new(2),
            before: Some(NodeId::new(1)),
        },
        Patch::Move {
            parent: NodeId::ROOT,
            child: NodeId::new(2),
            before: None,
        },
        Patch::SetProp {
            id: NodeId::new(1),
            prop: Prop::Label,
            value: PropValue::text("Containers"),
        },
        Patch::ClearProp {
            id: NodeId::new(1),
            prop: Prop::Label,
        },
        Patch::SetHandler {
            id: NodeId::new(1),
            handler: Handler::new(Trigger::Invoke, EventId::new("restart")),
        },
        Patch::ClearHandler {
            id: NodeId::new(1),
            trigger: Trigger::Invoke,
        },
        Patch::Remove { id: NodeId::new(1) },
    ];
    for patch in patches {
        assert_eq!(round_trip(&patch), patch, "{patch:?} did not round trip");
    }
}

#[cfg(feature = "wire")]
#[test]
fn a_composed_interface_survives_the_wire_whole() {
    let mut surface = Surface::new();
    let card = surface.create(Tag::Card);
    surface.append(NodeId::ROOT, card);
    let heading = surface.heading("Containers");
    surface.append(card, heading);
    let row = surface.container(Tag::Row, Length::Step(2));
    surface.append(card, row);
    let badge = surface.badge("running", Tone::Positive);
    surface.append(row, badge);
    let button = surface.button("Restart", EventId::new("restart"));
    surface.style(button, hl_gui::Variant::Filled, Tone::Danger);
    surface.append(row, button);
    let table = surface.table(SourceId::new(1));
    surface.set(
        table,
        Prop::Schema,
        PropValue::Schema(vec![Column::new("name", "Name")]),
    );
    surface.append(card, table);

    let frame = surface.frame();

    assert_eq!(
        round_trip(&frame),
        frame,
        "a whole interface must cross the wire unchanged"
    );
}

#[cfg(feature = "wire")]
#[test]
fn an_applied_wire_frame_produces_the_same_tree_as_a_local_one() {
    let mut surface = Surface::new();
    let column = surface.container(Tag::Column, Length::Step(2));
    surface.append(NodeId::ROOT, column);
    let text = surface.text("hello");
    surface.append(column, text);
    let frame = surface.frame();

    let decoded: Frame = round_trip(&frame);

    let mut local = hl_gui::Tree::new();
    let mut remote = hl_gui::Tree::new();
    local.apply(&frame, &mut Ignore).expect("applied locally");
    remote.apply(&decoded, &mut Ignore).expect("applied from the wire");

    assert_eq!(local.node(column), remote.node(column));
    assert_eq!(local.node(text), remote.node(text));
    assert_eq!(local.root().children, remote.root().children);
}

#[cfg(feature = "wire")]
#[test]
fn row_data_survives_the_wire() {
    let window = RowWindow {
        source: SourceId::new(1),
        version: Version::new(3),
        request: RequestId::new(7),
        range: RowRange::new(128, 128),
        rows: vec![Row::new(
            128,
            [
                Cell::text("api"),
                Cell::Number(1.5),
                Cell::Bytes(4096),
                Cell::badge("running", Tone::Positive),
                Cell::Stamp(1_700_000_000),
                Cell::Empty,
            ],
        )],
    };
    assert_eq!(round_trip(&window), window);

    let request = RowRequest {
        id: RequestId::new(7),
        source: SourceId::new(1),
        version: Version::new(3),
        range: RowRange::block(40_000),
        sort: Some(hl_gui::Sort {
            column: "name".into(),
            descending: true,
        }),
        filter: Some("alpine".into()),
    };
    assert_eq!(round_trip(&request), request);

    let mutation = SourceMutation::Invalidate {
        source: SourceId::new(1),
        version: Version::new(4),
        range: Some(RowRange::new(0, 128)),
    };
    assert_eq!(round_trip(&mutation), mutation);
}

#[cfg(feature = "wire")]
#[test]
fn a_theme_survives_the_wire() {
    let theme = Theme::dark();
    let decoded = round_trip(&theme);
    assert_eq!(decoded, theme);
    assert_eq!(decoded.density, Density::Normal);
}

#[cfg(feature = "wire")]
#[test]
fn table_schema_wire_refuses_duplicate_and_overlong_utf8_identity() {
    let column = serde_json::json!({
        "key": "same", "title": "Name", "width": "Content", "align": "Start",
        "sortable": false, "editable": false
    });
    let duplicate = serde_json::json!({"Schema": [column.clone(), column]});
    assert!(serde_json::from_value::<PropValue>(duplicate)
        .unwrap_err()
        .to_string()
        .contains("unique"));

    let overlong = serde_json::json!({"Schema": [{
        "key": "é".repeat(65), "title": "Name", "width": "Content", "align": "Start",
        "sortable": false, "editable": false
    }]});
    assert!(serde_json::from_value::<PropValue>(overlong)
        .unwrap_err()
        .to_string()
        .contains("column key"));
}

/// A renderer that records nothing, for tests that only care about the tree.
#[cfg(feature = "wire")]
struct Ignore;

#[cfg(feature = "wire")]
impl hl_gui::Renderer for Ignore {
    type Error = std::convert::Infallible;

    fn patch(&mut self, _patch: &Patch, _tree: &hl_gui::Tree) -> Result<(), Self::Error> {
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
