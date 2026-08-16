//! The interface this extension draws.
//!
//! Written as a description of what the interface should be, not as a sequence
//! of mutations. The reconciler works out the difference from what was drawn
//! last time, so a listing that changes one container's state costs one
//! property change rather than a rebuilt table.

use hl_gui::{
    Align, Element, EventId, Frame, Length, Prop, PropValue, Reconciliation, Scale, Tag, Tone, Trigger, Variant,
};

use crate::catalogue::Catalogue;
use crate::SOURCE;

/// Interactions this interface reports.
pub struct Actions;

impl Actions {
    /// The person asked for a fresh listing.
    #[must_use]
    pub fn refresh() -> EventId {
        EventId::new("containers.refresh")
    }

    /// The person typed in the filter.
    #[must_use]
    pub fn filter() -> EventId {
        EventId::new("containers.filter")
    }
}

/// Draws the container view.
pub struct View {
    reconciliation: Reconciliation,
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}

impl View {
    #[must_use]
    pub fn new() -> Self {
        Self {
            reconciliation: Reconciliation::new(),
        }
    }

    /// Produces the mutations that bring the drawn interface up to date.
    pub fn render(&mut self, catalogue: &Catalogue) -> Frame {
        self.reconciliation.reconcile(&Self::describe(catalogue))
    }

    /// The whole interface, as it should be for this listing.
    fn describe(catalogue: &Catalogue) -> Element {
        Element::column()
            .gap(Length::Step(3))
            .prop(Prop::Pad, PropValue::Length(Length::Step(4)))
            .child(Self::toolbar(catalogue))
            .child(Self::table())
    }

    /// Heading, live counts, filter, and the refresh action.
    fn toolbar(catalogue: &Catalogue) -> Element {
        Element::new(Tag::Toolbar)
            .prop(Prop::Pad, PropValue::Length(Length::Step(2)))
            .child(Element::heading("Containers").scale(Scale::Title))
            .child(Element::badge(Self::summary(catalogue), Self::tone(catalogue)).key("summary"))
            .child(Element::new(Tag::Spacer))
            .child(
                Element::new(Tag::Search)
                    .prop(Prop::Placeholder, PropValue::text("Filter containers…"))
                    .width(Length::Chars(24))
                    .on(Trigger::Change, Actions::filter()),
            )
            .child(
                Element::button("Refresh", Actions::refresh())
                    .variant(Variant::Outline)
                    .tone(Tone::Accent),
            )
    }

    /// The table, bound to the windowed source rather than to rows.
    fn table() -> Element {
        Element::new(Tag::DataTable)
            .prop(Prop::Source, PropValue::Source(SOURCE))
            .prop(Prop::Schema, PropValue::Schema(Self::schema()))
            .prop(Prop::Height, PropValue::Length(Length::Fill))
    }

    fn schema() -> Vec<hl_gui::Column> {
        vec![
            hl_gui::Column::new("name", "Name").width(Length::Fill).sortable(),
            hl_gui::Column::new("image", "Image").width(Length::Chars(28)),
            hl_gui::Column::new("state", "State").width(Length::Chars(14)),
            hl_gui::Column::new("created", "Created")
                .width(Length::Chars(16))
                .align(Align::End),
        ]
    }

    fn summary(catalogue: &Catalogue) -> String {
        format!("{} of {} running", catalogue.running(), catalogue.len())
    }

    /// The summary reads positive only when everything is up, so a stopped
    /// container is visible without reading the table.
    fn tone(catalogue: &Catalogue) -> Tone {
        if catalogue.is_empty() {
            return Tone::Neutral;
        }
        if catalogue.running() == catalogue.len() {
            return Tone::Positive;
        }
        Tone::Warning
    }
}

#[cfg(test)]
mod tests {
    use super::{Actions, View};
    use crate::catalogue::Catalogue;
    use hl_extension::port::ContainerSummary;
    use hl_gui::{Patch, Prop, PropValue, Tag, Tone};

    fn container(name: &str, state: &str) -> ContainerSummary {
        ContainerSummary {
            id: format!("id-{name}"),
            name: name.into(),
            image: "alpine:3.20".into(),
            state: state.into(),
            created: 0,
        }
    }

    fn listing(states: &[&str]) -> Catalogue {
        let mut catalogue = Catalogue::new();
        catalogue.replace(
            states
                .iter()
                .enumerate()
                .map(|(index, state)| container(&format!("c{index}"), state))
                .collect(),
        );
        catalogue
    }

    fn created(frame: &hl_gui::Frame) -> Vec<Tag> {
        frame
            .patches
            .iter()
            .filter_map(|patch| match patch {
                Patch::Create { tag, .. } => Some(*tag),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_view_is_composed_only_from_library_components() {
        let frame = View::new().render(&listing(&["running"]));

        for expected in [
            Tag::Column,
            Tag::Toolbar,
            Tag::Heading,
            Tag::Badge,
            Tag::Spacer,
            Tag::Search,
            Tag::Button,
            Tag::DataTable,
        ] {
            assert!(created(&frame).contains(&expected), "{expected:?} is missing");
        }
    }

    #[test]
    fn an_unchanged_listing_costs_nothing_to_redraw() {
        let catalogue = listing(&["running", "exited"]);
        let mut view = View::new();
        let first = view.render(&catalogue);
        assert!(!first.is_empty(), "the first description builds the interface");

        let second = view.render(&catalogue);

        assert!(
            second.is_empty(),
            "polling an unchanged listing must not redraw it, got {:?}",
            second.patches
        );
    }

    #[test]
    fn one_container_changing_state_costs_one_property() {
        let mut view = View::new();
        let _ = view.render(&listing(&["running", "running"]));

        let frame = view.render(&listing(&["running", "exited"]));

        assert_eq!(
            frame.patches.len(),
            2,
            "only the summary's text and tone should change, got {:?}",
            frame.patches
        );
        assert!(created(&frame).is_empty(), "nothing is rebuilt when a listing changes");
    }

    #[test]
    fn the_table_is_bound_to_a_source_rather_than_carrying_rows() {
        let catalogue = listing(&["running"; 500]);

        let frame = View::new().render(&catalogue);

        let bound = frame.patches.iter().any(|patch| {
            matches!(
                patch,
                Patch::SetProp {
                    prop: Prop::Source,
                    value: PropValue::Source(source),
                    ..
                } if *source == crate::SOURCE
            )
        });
        assert!(bound, "the table must name its source");
        assert!(
            frame.patches.len() < 60,
            "five hundred containers produced {} patches; rows belong in windows",
            frame.patches.len()
        );
    }

    #[test]
    fn the_summary_warns_when_something_is_not_running() {
        let frame = View::new().render(&listing(&["running", "exited"]));

        let toned = frame.patches.iter().any(|patch| {
            matches!(
                patch,
                Patch::SetProp {
                    prop: Prop::Tone,
                    value: PropValue::Tone(Tone::Warning),
                    ..
                }
            )
        });
        assert!(toned, "a stopped container must be visible without reading the table");
    }

    #[test]
    fn the_actions_the_view_reports_are_stable() {
        let frame = View::new().render(&listing(&["running"]));

        let declared: Vec<String> = frame
            .patches
            .iter()
            .filter_map(|patch| match patch {
                Patch::SetHandler { handler, .. } => Some(handler.id.as_str().to_owned()),
                _ => None,
            })
            .collect();

        assert!(declared.contains(&Actions::refresh().as_str().to_owned()));
        assert!(declared.contains(&Actions::filter().as_str().to_owned()));
    }
}
