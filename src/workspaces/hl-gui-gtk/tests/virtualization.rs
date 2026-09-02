//! The toolkit model must describe the whole source while holding a viewport.
//!
//! These run against real GTK types, so they need a display connection; when
//! none is available they report that rather than passing silently.

use gtk::prelude::*;
use hl_gui::{Cell, Row, RowRange, RowWindow, SourceId, Version};
use hl_gui_gtk::Rows;

const SOURCE: SourceId = SourceId::new(1);
const LENGTH: u64 = 1_000_000;

/// Initializes the toolkit, or reports that this environment cannot.
fn toolkit() -> bool {
    gtk::init().is_ok()
}

fn answer(request: &hl_gui::RowRequest) -> RowWindow {
    let rows = (0..request.range.count)
        .map(|offset| {
            let index = request.range.start + u64::from(offset);
            Row::new(index, [Cell::text(format!("row {index}")), Cell::Bytes(index * 1024)])
        })
        .collect();
    RowWindow {
        source: request.source,
        version: Version::new(1),
        request: request.id,
        range: request.range,
        rows,
    }
}

/// Every scenario runs inside one test.
///
/// GTK may only be initialized from a single thread, and the libtest harness
/// gives every `#[test]` its own thread — so a second GTK test in the same
/// binary either panics or, if it treats the failure as "no display", skips
/// itself without saying anything useful. One test that runs the scenarios in
/// sequence is therefore the only shape in which all of them actually run.
#[test]
fn a_model_virtualizes_a_source_larger_than_the_widgets_that_show_it() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
    let scenarios: [(&str, fn()); 8] = [
        (
            "a_model_describes_the_whole_source_while_holding_a_viewport",
            a_model_describes_the_whole_source_while_holding_a_viewport,
        ),
        (
            "realizing_a_row_answers_at_once_and_asks_for_the_rest",
            realizing_a_row_answers_at_once_and_asks_for_the_rest,
        ),
        (
            "a_delivered_window_replaces_the_placeholders_it_covers",
            a_delivered_window_replaces_the_placeholders_it_covers,
        ),
        (
            "an_oversized_window_never_reaches_the_gtk_model",
            an_oversized_window_never_reaches_the_gtk_model,
        ),
        (
            "scrolling_a_large_source_holds_only_a_bounded_number_of_rows",
            scrolling_a_large_source_holds_only_a_bounded_number_of_rows,
        ),
        (
            "an_invalidated_band_returns_to_pending_without_disturbing_the_rest",
            an_invalidated_band_returns_to_pending_without_disturbing_the_rest,
        ),
        (
            "a_window_arriving_before_a_length_is_still_reachable",
            a_window_arriving_before_a_length_is_still_reachable,
        ),
        (
            "a_real_column_view_resizes_without_materializing_the_logical_source",
            a_real_column_view_resizes_without_materializing_the_logical_source,
        ),
    ];
    let mut ran = 0;
    for (name, scenario) in scenarios {
        scenario();
        eprintln!("ran {name}");
        ran += 1;
    }
    assert_eq!(ran, scenarios.len(), "every scenario must actually execute");
}

fn an_oversized_window_never_reaches_the_gtk_model() {
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);
    let _ = rows.item(0);
    let request = rows.drain().remove(0);
    let mut oversized = answer(&request);
    oversized.rows.push(Row::new(
        u64::from(request.range.count),
        [Cell::text("outside request")],
    ));

    rows.deliver(&oversized);
    assert_eq!(rows.held(), 0, "invalid rows never enter the GTK model");
    assert!(rows.is_pending(0), "the requested row remains pending");

    rows.deliver(&answer(&request));
    assert!(!rows.is_pending(0), "the valid response can still recover");
}

fn a_real_column_view_resizes_without_materializing_the_logical_source() {
    const STORY_ROWS: u64 = 100_000;
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), STORY_ROWS);
    let selection = gtk::NoSelection::new(Some(rows.clone()));
    let view = gtk::ColumnView::new(Some(selection));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
        item.set_child(Some(&gtk::Label::new(None)));
    });
    factory.connect_bind(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
        let label = item.child().and_downcast::<gtk::Label>().expect("label");
        let value = item.item().and_downcast::<gtk::StringObject>().expect("row");
        label.set_text(&value.string());
    });
    view.append_column(&gtk::ColumnViewColumn::new(Some("Record"), Some(factory)));
    let scroll = gtk::ScrolledWindow::builder().child(&view).build();
    let window = gtk::Window::builder()
        .child(&scroll)
        .default_width(640)
        .default_height(320)
        .build();
    window.present();
    settle();
    let compact = descendants(window.clone().upcast_ref()).len();
    window.set_default_size(1200, 720);
    settle();
    let expanded = descendants(window.clone().upcast_ref()).len();

    assert_eq!(u64::from(rows.n_items()), STORY_ROWS);
    assert!(compact < 1_000, "a 100k-row view materialized {compact} GTK widgets");
    assert!(expanded < 2_000, "resizing materialized {expanded} GTK widgets");
    assert!(rows.held() <= hl_gui::RowCache::CAPACITY);
    window.close();
}

fn settle() {
    let context = gtk::glib::MainContext::default();
    for _ in 0..20 {
        while context.pending() {
            context.iteration(false);
        }
    }
}

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(widget) = pending.pop() {
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            pending.push(current.clone());
            found.push(current);
        }
    }
    found
}

fn a_model_describes_the_whole_source_while_holding_a_viewport() {
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);

    assert_eq!(
        u64::from(rows.n_items()),
        LENGTH,
        "the scrollbar must describe the source, not the cache"
    );
    assert_eq!(rows.held(), 0, "nothing is held before anything is asked for");
}

fn realizing_a_row_answers_at_once_and_asks_for_the_rest() {
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);

    let item = rows.item(500_000).expect("an item is always produced");
    let text = item.downcast::<gtk::StringObject>().expect("a string object").string();
    assert_eq!(text, "…", "a miss renders a placeholder rather than blocking");

    let requests = rows.drain();
    assert!(
        requests.iter().any(|request| request.range.contains(500_000)),
        "realizing a row schedules the window containing it"
    );
}

fn a_delivered_window_replaces_the_placeholders_it_covers() {
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);
    let _ = rows.item(0);

    for request in rows.drain() {
        rows.deliver(&answer(&request));
    }

    let item = rows.item(0).expect("an item");
    let text = item.downcast::<gtk::StringObject>().expect("a string object").string();
    assert!(text.starts_with("row 0"), "expected the delivered row, got {text}");
    assert!(text.contains('\u{1f}'), "cells stay separated for the column factory");
    assert!(!rows.is_pending(0));
}

fn scrolling_a_large_source_holds_only_a_bounded_number_of_rows() {
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);

    let mut index = 0;
    let mut now = 0;
    while index < 200_000 {
        rows.tick(now);
        let _ = rows.item(index);
        for request in rows.drain() {
            rows.deliver(&answer(&request));
        }
        index += 512;
        now += 1;
    }

    assert!(
        rows.held() <= hl_gui::RowCache::CAPACITY,
        "held {} rows while scrolling a million-row source",
        rows.held()
    );
    assert_eq!(u64::from(rows.n_items()), LENGTH, "the source length is unchanged");
}

fn an_invalidated_band_returns_to_pending_without_disturbing_the_rest() {
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);
    for index in [0_u32, 512] {
        let _ = rows.item(index);
        for request in rows.drain() {
            rows.deliver(&answer(&request));
        }
    }
    assert!(!rows.is_pending(0));
    assert!(!rows.is_pending(512));

    rows.invalidate(Version::new(1), Some(RowRange::new(512, 8)));

    assert!(rows.is_pending(512), "the invalidated band is refetched");
    assert!(!rows.is_pending(0), "an unrelated band keeps its widgets");
}

fn a_window_arriving_before_a_length_is_still_reachable() {
    let rows = Rows::new(SOURCE);
    let _ = rows.item(0);
    let requests = rows.drain();

    // Realizing a row observes a viewport, and the cache bounds a request by
    // the length only once it knows one — so the first block is already on its
    // way. This scenario never ran while each test had its own thread, and the
    // assertion it carried (that nothing was scheduled) contradicted the
    // windowing contract `hl-gui` pins in `filtering_invalidates_the_row_count`.
    assert!(
        !requests.is_empty(),
        "a realized row schedules its block even before a length is known"
    );
    for request in requests {
        rows.deliver(&answer(&request));
    }

    // The delivered window opened generation 1, so the resize that follows is
    // not a new generation and does not discard what already arrived.
    rows.resize(Version::new(1), 128);
    let _ = rows.item(0);
    for request in rows.drain() {
        rows.deliver(&answer(&request));
    }
    assert!(!rows.is_pending(0));
}
