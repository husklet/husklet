//! The interface this extension draws.
//!
//! Composed entirely from the component library, so it renders identically
//! wherever the host runs and needs no toolkit of its own.

use hl_gui::{EventId, Frame, Length, NodeId, Prop, PropValue, Scale, Surface, Tag, Tone, Trigger, Variant};

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
    surface: Surface,
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
            surface: Surface::new(),
        }
    }

    /// Describes the whole interface as one frame.
    pub fn render(&mut self, catalogue: &Catalogue) -> Frame {
        let page = self.surface.container(Tag::Column, Length::Step(3));
        self.surface.set(page, Prop::Pad, PropValue::Length(Length::Step(4)));
        self.surface.append(NodeId::ROOT, page);

        let toolbar = self.toolbar(catalogue);
        self.surface.append(page, toolbar);

        let table = self.table();
        self.surface.append(page, table);

        self.surface.frame()
    }

    /// Heading, live counts, filter, and the refresh action.
    fn toolbar(&mut self, catalogue: &Catalogue) -> NodeId {
        let bar = self.surface.create(Tag::Toolbar);
        self.surface.set(bar, Prop::Pad, PropValue::Length(Length::Step(2)));

        let heading = self.surface.heading("Containers");
        self.surface.set(heading, Prop::Scale, PropValue::Scale(Scale::Title));
        self.surface.append(bar, heading);

        let count = self.surface.badge(Self::summary(catalogue), Self::tone(catalogue));
        self.surface.append(bar, count);

        let spacer = self.surface.create(Tag::Spacer);
        self.surface.append(bar, spacer);

        let search = self.surface.create(Tag::Search);
        self.surface
            .set(search, Prop::Placeholder, PropValue::text("Filter containers…"));
        self.surface
            .set(search, Prop::Width, PropValue::Length(Length::Chars(24)));
        self.surface.on(search, Trigger::Change, Actions::filter());
        self.surface.append(bar, search);

        let refresh = self.surface.button("Refresh", Actions::refresh());
        self.surface.style(refresh, Variant::Outline, Tone::Accent);
        self.surface.append(bar, refresh);
        bar
    }

    /// The table itself, bound to the windowed source rather than to rows.
    fn table(&mut self) -> NodeId {
        let table = self.surface.table(SOURCE);
        self.surface.set(table, Prop::Schema, PropValue::Schema(Self::schema()));
        self.surface.set(table, Prop::Height, PropValue::Length(Length::Fill));
        table
    }

    fn schema() -> Vec<hl_gui::Column> {
        vec![
            hl_gui::Column::new("name", "Name").width(Length::Fill).sortable(),
            hl_gui::Column::new("image", "Image").width(Length::Chars(28)),
            hl_gui::Column::new("state", "State").width(Length::Chars(14)),
            hl_gui::Column::new("created", "Created")
                .width(Length::Chars(16))
                .align(hl_gui::Align::End),
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
    use hl_gui::{Patch, Prop, PropValue, Tag, Tone};
    use hl_ws_extension::port::ContainerSummary;

    fn container(name: &str, state: &str) -> ContainerSummary {
        ContainerSummary {
            id: format!("id-{name}"),
            name: name.into(),
            image: "alpine:3.20".into(),
            state: state.into(),
            created: 0,
        }
    }

    fn tags(frame: &hl_gui::Frame) -> Vec<Tag> {
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
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![container("api", "running")]);

        let frame = View::new().render(&catalogue);
        let created = tags(&frame);

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
            assert!(created.contains(&expected), "{expected:?} is missing from the view");
        }
    }

    #[test]
    fn the_table_is_bound_to_a_source_rather_than_carrying_rows() {
        let mut catalogue = Catalogue::new();
        catalogue.replace(
            (0..500)
                .map(|index| container(&format!("c{index}"), "running"))
                .collect(),
        );

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
            "five hundred containers produced {} patches; rows belong in windows, not the interface",
            frame.patches.len()
        );
    }

    #[test]
    fn the_summary_warns_when_something_is_not_running() {
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![container("api", "running"), container("db", "exited")]);

        let frame = View::new().render(&catalogue);

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
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![container("api", "running")]);
        let frame = View::new().render(&catalogue);

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
