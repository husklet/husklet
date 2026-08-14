//! A deliberately hard screen: the kind of interface a database extension
//! needs, built only from what the library actually offers today.
//!
//! This story exists to be judged rather than admired. Where it looks clumsy,
//! the library is missing something, and the comment beside it says what.

use hl_gui::{Align, Cell, Length, NodeId, Orientation, Prop, PropValue, Row, Scale, Surface, Tag, Tone, Trigger};

use super::Sample;

/// The workbench draws from its own source: two tables sharing one identifier
/// would each be answered with the other's rows.
pub const SOURCE: hl_gui::SourceId = hl_gui::SourceId::new(2);

/// The schema, the editor, and the results, side by side.
pub(super) fn workbench(surface: &mut Surface, parent: NodeId) {
    let split = surface.create(Tag::Splitter);
    surface.set(split, Prop::Position, PropValue::Number(220.0));
    surface.set(split, Prop::Height, PropValue::Length(Length::Step(12)));
    surface.append(parent, split);

    let schema = catalogue(surface);
    surface.append(split, schema);

    let workspace = surface.container(Tag::Column, Length::Step(2));
    surface.append(split, workspace);
    let editor = editor(surface);
    surface.append(workspace, editor);
    let results = results(surface);
    surface.append(workspace, results);
}

/// The schema browser.
///
/// A real one is a tree: database, then schemas, then tables, then columns,
/// each expandable. The library has no tree, so this is a flat list of
/// expanders — which loses the hierarchy below two levels and cannot show a
/// column without a third.
fn catalogue(surface: &mut Surface) -> NodeId {
    let column = surface.container(Tag::Column, Length::Step(1));
    surface.set(column, Prop::Pad, PropValue::Length(Length::Step(2)));

    let heading = surface.text("SCHEMA");
    surface.set(heading, Prop::Scale, PropValue::Scale(Scale::Caption));
    surface.append(column, heading);

    for (schema, tables) in [
        ("public", ["users", "orders", "order_items"]),
        ("billing", ["invoices", "payments", "refunds"]),
    ] {
        let group = surface.create(Tag::Expander);
        surface.set(group, Prop::Label, PropValue::text(schema));
        surface.set(group, Prop::Expanded, PropValue::Flag(true));
        surface.append(column, group);

        let inner = surface.container(Tag::Column, Length::Step(0));
        surface.set(inner, Prop::Pad, PropValue::Length(Length::Step(1)));
        surface.append(group, inner);
        for table in tables {
            let entry = surface.text(table);
            surface.set(entry, Prop::Align, PropValue::Align(Align::Start));
            surface.append(inner, entry);
        }
    }
    column
}

/// The query editor and the actions that run it.
///
/// The text area is plain: no line numbers, no gutter, no syntax colour, and
/// no way to say where the caret is. Anything beyond typing a statement needs
/// a component the library does not have.
fn editor(surface: &mut Surface) -> NodeId {
    let column = surface.container(Tag::Column, Length::Step(1));
    surface.set(column, Prop::Pad, PropValue::Length(Length::Step(2)));

    let bar = surface.create(Tag::Toolbar);
    surface.append(column, bar);

    let run = surface.button("Run", Sample::event("query.run"));
    surface.style(run, hl_gui::Variant::Filled, Tone::Accent);
    surface.append(bar, run);

    let explain = surface.button("Explain", Sample::event("query.explain"));
    surface.style(explain, hl_gui::Variant::Outline, Tone::Neutral);
    surface.append(bar, explain);

    let spacer = surface.create(Tag::Spacer);
    surface.append(bar, spacer);

    let elapsed = surface.badge("42 ms · 5 rows", Tone::Positive);
    surface.append(bar, elapsed);

    let query = surface.create(Tag::TextArea);
    surface.set(
        query,
        Prop::Value,
        PropValue::text("select id, email, created_at\n  from public.users\n where created_at > now() - interval '7 days'\n order by created_at desc;"),
    );
    surface.set(query, Prop::Monospace, PropValue::Flag(true));
    surface.on(query, Trigger::Change, Sample::event("query.edit"));
    surface.append(column, query);
    column
}

/// The result grid.
///
/// This part the library does well: the table names a source and the host
/// fetches windows, so a result set of any size costs one viewport.
fn results(surface: &mut Surface) -> NodeId {
    let column = surface.container(Tag::Column, Length::Step(1));
    surface.set(column, Prop::Pad, PropValue::Length(Length::Step(2)));

    let divider = surface.create(Tag::Separator);
    surface.set(divider, Prop::Width, PropValue::Length(Length::Fill));
    surface.append(column, divider);

    let table = surface.table(SOURCE);
    surface.set(table, Prop::Schema, PropValue::Schema(schema()));
    surface.set(table, Prop::Height, PropValue::Length(Length::Fill));
    surface.append(column, table);

    // A status bar wants its parts pushed apart and pinned to the bottom.
    // There is no bottom bar and no per-side padding, so this is a row with a
    // spacer doing the work a layout should do.
    let status = surface.container(Tag::Row, Length::Step(2));
    surface.set(
        status,
        Prop::Orientation,
        PropValue::Orientation(Orientation::Horizontal),
    );
    surface.append(column, status);
    let connection = surface.text("postgres://localhost:5432/app");
    surface.set(connection, Prop::Scale, PropValue::Scale(Scale::Caption));
    surface.append(status, connection);
    let gap = surface.create(Tag::Spacer);
    surface.append(status, gap);
    let state = surface.badge("connected", Tone::Positive);
    surface.append(status, state);
    column
}

fn schema() -> Vec<hl_gui::Column> {
    vec![
        hl_gui::Column::new("id", "id")
            .width(Length::Chars(8))
            .align(Align::End),
        hl_gui::Column::new("email", "email").width(Length::Fill).sortable(),
        hl_gui::Column::new("created_at", "created_at").width(Length::Chars(22)),
        hl_gui::Column::new("state", "state").width(Length::Chars(12)),
    ]
}

/// Rows the workbench answers window requests with.
#[must_use]
pub fn row(index: u64) -> Row {
    let states = [
        ("active", Tone::Positive),
        ("pending", Tone::Warning),
        ("closed", Tone::Neutral),
    ];
    let (state, tone) = states[(index % states.len() as u64) as usize];
    Row::new(
        index,
        [
            Cell::Number(index as f64 + 1.0),
            Cell::text(format!("person{index:04}@example.com")),
            Cell::Stamp(1_700_000_000 + index as i64 * 3_600),
            Cell::badge(state, tone),
        ],
    )
}
