use hl_gui::{Cell, Length, NodeId, Prop, PropValue, Row, RowRange, RowWindow, Surface, Tag, Tone};

use super::Sample;

/// A table bound to a windowed source. Rows arrive through the renderer, not
/// through the node tree, which is what keeps a large result set affordable.
pub(super) fn table(surface: &mut Surface, parent: NodeId) {
    let table = surface.table(Sample::source());
    surface.set(table, Prop::Schema, PropValue::Schema(Sample::schema()));
    surface.set(table, Prop::Height, PropValue::Length(Length::Step(12)));
    surface.append(parent, table);
}

/// The rows the storybook answers its own window request with.
#[must_use]
pub fn window() -> RowWindow {
    let rows = [
        ("api", "husklet/api:1.4.2", "running", 184_549_376_u64, Tone::Positive),
        ("worker", "husklet/worker:1.4.2", "running", 96_468_992, Tone::Positive),
        (
            "postgres",
            "postgres:16-alpine",
            "restarting",
            251_658_240,
            Tone::Warning,
        ),
        ("redis", "redis:7-alpine", "exited", 41_943_040, Tone::Danger),
        ("migrate", "husklet/migrate:1.4.2", "created", 12_582_912, Tone::Neutral),
    ];
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(index, (name, image, state, size, tone))| {
            Row::new(
                index as u64,
                [
                    Cell::text(name),
                    Cell::text(image),
                    Cell::badge(state, tone),
                    Cell::Bytes(size),
                ],
            )
        })
        .collect();
    RowWindow {
        source: Sample::source(),
        version: hl_gui::Version::new(1),
        request: hl_gui::RequestId::new(1),
        range: RowRange::new(0, 5),
        rows,
    }
}

/// Composed list rows: each is a real node tree, not a formatted string.
pub(super) fn list(surface: &mut Surface, parent: NodeId) {
    let list = surface.create(Tag::List);
    surface.set(list, Prop::Height, PropValue::Length(Length::Step(12)));
    surface.append(parent, list);

    for (name, detail, tone) in [
        ("alpine:3.20", "7.8 MB · 2 days ago", Tone::Neutral),
        ("postgres:16-alpine", "240 MB · 6 hours ago", Tone::Accent),
        ("husklet/api:1.4.2", "176 MB · 20 minutes ago", Tone::Positive),
    ] {
        let row = surface.create(Tag::ListRow);
        surface.set(row, Prop::Pad, PropValue::Length(Length::Step(2)));
        surface.append(list, row);

        let text = surface.container(Tag::Column, Length::Step(0));
        surface.append(row, text);
        let title = surface.text(name);
        surface.append(text, title);
        let caption = surface.text(detail);
        surface.set(caption, Prop::Scale, PropValue::Scale(hl_gui::Scale::Caption));
        surface.append(text, caption);

        let spacer = surface.create(Tag::Spacer);
        surface.append(row, spacer);

        let badge = surface.badge("local", tone);
        surface.append(row, badge);
    }
}
