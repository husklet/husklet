//! A settings screen built from composite parts rather than primitives.
//!
//! This is the case the library was widened for: a producer that can say
//! "this is the card's header, this is its action row, this is the helper text
//! under that field" rather than pushing children into a box and hoping the
//! adapter guesses. Every part here is a named component.

use hl_gui::{Element, EventId, Length, NodeId, Prop, PropValue, Scale, Surface, Tag, Tone, Trigger, Variant};

/// Composes the screen and applies it through the reconciler, so the story is
/// written the way an extension writes one.
pub(super) fn screen(surface: &mut Surface, parent: NodeId) {
    let mut reconciliation = hl_gui::Reconciliation::new();
    let frame = reconciliation.reconcile(&describe());
    // The catalogue builds through a Surface, so the described tree is replayed
    // into it: same components, same order, one identity space.
    replay(surface, parent, &frame);
}

/// The whole screen, as a description.
fn describe() -> Element {
    Element::column()
        .gap(Length::Step(3))
        .child(connection())
        .child(resources())
        .child(advanced())
}

/// A card whose header, body and actions are each named.
fn connection() -> Element {
    Element::new(Tag::Card)
        .child(
            Element::new(Tag::CardHeader)
                .child(Element::heading("Connection").scale(Scale::Title))
                .child(Element::badge("verified", Tone::Positive)),
        )
        .child(
            Element::new(Tag::CardContent).child(
                Element::new(Tag::FormGroup)
                    .child(field("Host", "db.internal", "The address the workspace dials."))
                    .child(field("Port", "5432", "5432 unless the server was moved."))
                    .child(field("Database", "app", "Opened on connect.")),
            ),
        )
        .child(
            Element::new(Tag::CardActions)
                .child(Element::button("Test", EventId::new("connection.test")).variant(Variant::Outline))
                .child(
                    Element::button("Save", EventId::new("connection.save"))
                        .variant(Variant::Filled)
                        .tone(Tone::Accent),
                ),
        )
}

/// One labelled field with its helper text — three named parts, not a guess.
fn field(label: &str, value: &str, help: &str) -> Element {
    Element::new(Tag::FormControl)
        .child(Element::new(Tag::FormLabel).label(label))
        .child(Element::entry(value, EventId::new("connection.edit")))
        .child(Element::new(Tag::FormHelperText).label(help))
}

/// Figures presented as figures, rather than as sentences in a label.
fn resources() -> Element {
    Element::new(Tag::Card).child(
        Element::new(Tag::CardContent).child(
            Element::row()
                .gap(Length::Step(4))
                .child(stat("Memory", "512 MiB", Tone::Neutral))
                .child(stat("Connections", "18 / 100", Tone::Positive))
                .child(stat("Replication lag", "4.2 s", Tone::Warning)),
        ),
    )
}

fn stat(caption: &str, figure: &str, tone: Tone) -> Element {
    Element::new(Tag::Stat)
        .child(Element::text(caption).scale(Scale::Caption))
        .child(Element::heading(figure).scale(Scale::Title).tone(tone))
}

/// Rarely-touched settings, folded away behind their own summary.
fn advanced() -> Element {
    Element::new(Tag::Accordion)
        .prop(Prop::Expanded, PropValue::Flag(true))
        .child(Element::new(Tag::AccordionSummary).child(Element::heading("Advanced")))
        .child(
            Element::new(Tag::AccordionDetails).child(
                Element::new(Tag::FormGroup)
                    .child(toggle("Reconnect automatically", true))
                    .child(toggle("Log every statement", false))
                    .child(toggle("Read only", false)),
            ),
        )
        .child(
            Element::new(Tag::AccordionActions)
                .child(Element::button("Reset", EventId::new("advanced.reset")).tone(Tone::Danger)),
        )
}

fn toggle(label: &str, on: bool) -> Element {
    Element::new(Tag::FormControlLabel)
        .child(
            Element::new(Tag::Switch)
                .prop(Prop::Checked, PropValue::Flag(on))
                .on(Trigger::Toggle, EventId::new("advanced.toggle")),
        )
        .child(Element::new(Tag::FormLabel).label(label))
}

/// Replays a described frame into the catalogue's surface.
///
/// The reconciler allocates its own identities, so they are remapped onto the
/// surface's rather than assumed to be free.
fn replay(surface: &mut Surface, parent: NodeId, frame: &hl_gui::Frame) {
    let mut mapping = std::collections::BTreeMap::new();
    for patch in &frame.patches {
        apply(surface, parent, &mut mapping, patch);
    }
}

fn apply(
    surface: &mut Surface,
    parent: NodeId,
    mapping: &mut std::collections::BTreeMap<NodeId, NodeId>,
    patch: &hl_gui::Patch,
) {
    match patch {
        hl_gui::Patch::Create { id, tag } => {
            let created = surface.create(*tag);
            mapping.insert(*id, created);
        }
        hl_gui::Patch::Insert {
            parent: host, child, ..
        } => {
            let (Some(child), host) = (mapping.get(child).copied(), mapping.get(host).copied()) else {
                return;
            };
            surface.append(host.unwrap_or(parent), child);
        }
        hl_gui::Patch::SetProp { id, prop, value } => {
            let Some(node) = mapping.get(id).copied() else {
                return;
            };
            surface.set(node, *prop, value.clone());
        }
        hl_gui::Patch::SetHandler { id, handler } => {
            let Some(node) = mapping.get(id).copied() else {
                return;
            };
            surface.on(node, handler.trigger, handler.id.clone());
        }
        _ => {}
    }
}
