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

/// The schema browser, as an actual tree.
///
/// Database, schema, table, column — four levels, each disclosed in place.
/// Until the library had a tree this was a flat list of expanders that lost
/// the hierarchy below two levels and could not show a column at all.
fn catalogue(surface: &mut Surface) -> NodeId {
    let tree = surface.create(Tag::Tree);
    surface.set(tree, Prop::Pad, PropValue::Length(Length::Step(2)));

    let database = branch(surface, "app", true);
    surface.append(tree, database);
    for (schema, tables) in SCHEMAS {
        let level = branch(surface, schema, *schema == "public");
        surface.append(database, level);
        tables_of(surface, level, tables);
    }
    tree
}

/// The tables of one schema, each disclosing its columns.
fn tables_of(surface: &mut Surface, level: NodeId, tables: &[Table]) {
    for (table, columns) in tables {
        let entry = branch(surface, table, *table == "users");
        surface.append(level, entry);
        columns_of(surface, entry, columns);
    }
}

/// The columns of one table — the level a flat list of expanders could not
/// reach at all.
fn columns_of(surface: &mut Surface, table: NodeId, columns: &[&str]) {
    for column in columns {
        let leaf = branch(surface, column, false);
        surface.append(table, leaf);
    }
}

/// One table and the columns it holds.
type Table = (&'static str, &'static [&'static str]);
/// One schema and the tables it holds.
type Schema = (&'static str, &'static [Table]);

/// The schema this story browses.
const SCHEMAS: &[Schema] = &[
    (
        "public",
        &[
            ("users", &["id", "email", "created_at"]),
            ("orders", &["id", "user_id", "total"]),
        ],
    ),
    (
        "billing",
        &[
            ("invoices", &["id", "issued_at", "amount"]),
            ("payments", &["id", "invoice_id", "paid_at"]),
        ],
    ),
];

/// One node of the tree, disclosed or folded.
fn branch(surface: &mut Surface, label: &str, open: bool) -> NodeId {
    let item = surface.create(Tag::TreeItem);
    surface.set(item, Prop::Label, PropValue::text(label));
    surface.set(item, Prop::Expanded, PropValue::Flag(open));
    item
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
