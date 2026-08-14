use hl_gui::{Cell, Length, NodeId, Prop, PropValue, Row, RowRequest, RowWindow, Surface, Tag, Tone, Version};

use super::Sample;

/// A table bound to a windowed source. Rows arrive through the renderer, not
/// through the node tree, which is what keeps a large result set affordable.
pub(super) fn table(surface: &mut Surface, parent: NodeId) {
    let table = surface.table(Sample::source());
    surface.set(table, Prop::Schema, PropValue::Schema(Sample::schema()));
    surface.set(table, Prop::Height, PropValue::Length(Length::Step(12)));
    surface.append(parent, table);
}

/// How many rows the catalogue's table claims to have.
///
/// Far more than could be held, so the story demonstrates virtualization
/// rather than merely displaying a handful of rows.
pub const ROWS: u64 = 100_000;

/// Answers one window request, the way an out-of-process producer would.
#[must_use]
pub fn answer(request: &RowRequest) -> RowWindow {
    let rows = (0..request.range.count)
        .map(|offset| {
            let index = request.range.start + u64::from(offset);
            // Each table answers from its own source; sharing one identifier
            // would show every table the first table's rows.
            if request.source == crate::story::database::SOURCE {
                return crate::story::database::row(index);
            }
            row(index)
        })
        .filter(|row| row.key < ROWS)
        .collect();
    RowWindow {
        source: request.source,
        version: Version::new(1),
        request: request.id,
        range: request.range,
        rows,
    }
}

fn row(index: u64) -> Row {
    let states = [
        ("running", Tone::Positive),
        ("restarting", Tone::Warning),
        ("exited", Tone::Danger),
        ("created", Tone::Neutral),
    ];
    let (state, tone) = states[(index % states.len() as u64) as usize];
    let images = [
        "husklet/api:1.4.2",
        "postgres:16-alpine",
        "redis:7-alpine",
        "alpine:3.20",
    ];
    Row::new(
        index,
        [
            Cell::text(format!("container-{index:05}")),
            Cell::text(images[(index % images.len() as u64) as usize]),
            Cell::badge(state, tone),
            Cell::Bytes(12_582_912 + index * 7_919),
        ],
    )
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
