use hl_gui::{
    Align, Column, EventId, Frame, Length, NodeId, Prop, PropValue, Scale, SourceId, Surface, Tag, Tone, Variant,
};

mod collection;
mod control;
mod display;
mod layout;

pub(crate) use collection::{answer, ROWS};

/// One catalogue entry: a titled sample of a component family.
pub struct Story {
    pub title: &'static str,
    pub summary: &'static str,
    build: fn(&mut Surface, NodeId),
}

impl Story {
    /// Renders this story's sample into `parent`.
    pub fn compose(&self, surface: &mut Surface, parent: NodeId) {
        (self.build)(surface, parent);
    }

    /// Whether a case-insensitive filter selects this story. No filter selects
    /// every story.
    #[must_use]
    pub fn matches(&self, filter: Option<&str>) -> bool {
        filter.is_none_or(|needle| self.title.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()))
    }
}

/// Every story, in presentation order.
pub struct Catalogue;

impl Catalogue {
    pub const STORIES: &'static [Story] = &[
        Story {
            title: "Typography",
            summary: "Text scales, code, and links",
            build: display::typography,
        },
        Story {
            title: "Status",
            summary: "Badges, avatars, progress, spinner",
            build: display::status,
        },
        Story {
            title: "Buttons",
            summary: "Every variant against every tone",
            build: control::buttons,
        },
        Story {
            title: "Inputs",
            summary: "Entry, search, number, multi-line",
            build: control::inputs,
        },
        Story {
            title: "Selection",
            summary: "Switch, checkbox, radio, dropdown, slider",
            build: control::selection,
        },
        Story {
            title: "Layout",
            summary: "Rows, columns, spacing, separators",
            build: layout::spacing,
        },
        Story {
            title: "Surfaces",
            summary: "Cards, toolbars, expanders, banners",
            build: layout::surfaces,
        },
        Story {
            title: "Data table",
            summary: "Windowed rows with typed cells",
            build: collection::table,
        },
        Story {
            title: "List",
            summary: "Composed rows with trailing actions",
            build: collection::list,
        },
    ];

    /// Builds the whole catalogue as one frame.
    #[must_use]
    pub fn frame() -> (Surface, Frame) {
        Self::selected(None)
    }

    /// Builds only the stories whose title contains `filter`, so one component
    /// family can be reviewed without rendering the whole catalogue.
    #[must_use]
    pub fn selected(filter: Option<&str>) -> (Surface, Frame) {
        let mut surface = Surface::new();
        let page = surface.container(Tag::Column, Length::Step(6));
        surface.set(page, Prop::Pad, PropValue::Length(Length::Step(6)));
        surface.append(NodeId::ROOT, page);

        let title = surface.create(Tag::Heading);
        surface.set(title, Prop::Label, PropValue::text("Component catalogue"));
        surface.set(title, Prop::Scale, PropValue::Scale(Scale::Display));
        surface.append(page, title);

        for story in Self::STORIES {
            if !story.matches(filter) {
                continue;
            }
            let section = Self::section(&mut surface, page, story);
            story.compose(&mut surface, section);
        }
        let frame = surface.frame();
        (surface, frame)
    }

    /// A titled card holding one story's sample.
    fn section(surface: &mut Surface, page: NodeId, story: &Story) -> NodeId {
        let card = surface.create(Tag::Card);
        surface.set(card, Prop::Pad, PropValue::Length(Length::Step(4)));
        surface.append(page, card);

        let body = surface.container(Tag::Column, Length::Step(3));
        surface.set(body, Prop::Pad, PropValue::Length(Length::Step(3)));
        surface.append(card, body);

        let heading = surface.heading(story.title);
        surface.set(heading, Prop::Scale, PropValue::Scale(Scale::Title));
        surface.append(body, heading);

        let summary = surface.text(story.summary);
        surface.set(summary, Prop::Scale, PropValue::Scale(Scale::Caption));
        surface.append(body, summary);

        let sample = surface.container(Tag::Column, Length::Step(3));
        surface.append(body, sample);
        sample
    }
}

/// Shared helpers the individual stories compose with.
pub(crate) struct Sample;

impl Sample {
    /// A horizontal strip of samples with consistent spacing.
    pub(crate) fn strip(surface: &mut Surface, parent: NodeId) -> NodeId {
        let row = surface.container(Tag::Row, Length::Step(2));
        surface.set(row, Prop::Justify, PropValue::Align(Align::Center));
        surface.append(parent, row);
        row
    }

    /// A labelled sample: caption above, content below.
    pub(crate) fn labelled(surface: &mut Surface, parent: NodeId, caption: &str) -> NodeId {
        let column = surface.container(Tag::Column, Length::Step(1));
        surface.append(parent, column);
        let label = surface.text(caption);
        surface.set(label, Prop::Scale, PropValue::Scale(Scale::Caption));
        surface.append(column, label);
        column
    }

    /// The columns and rows used by both collection stories.
    pub(crate) fn schema() -> Vec<Column> {
        vec![
            Column::new("name", "Name").width(Length::Fill).sortable(),
            Column::new("image", "Image").width(Length::Chars(22)),
            Column::new("state", "State").width(Length::Chars(14)),
            Column::new("size", "Size").width(Length::Chars(10)).align(Align::End),
        ]
    }

    pub(crate) fn source() -> SourceId {
        SourceId::new(1)
    }

    pub(crate) fn event(name: &str) -> EventId {
        EventId::new(name)
    }

    pub(crate) const TONES: &'static [Tone] = Tone::ALL;
    pub(crate) const VARIANTS: &'static [Variant] = Variant::ALL;
}
