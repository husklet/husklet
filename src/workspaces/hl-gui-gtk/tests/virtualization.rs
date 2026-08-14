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

#[test]
fn a_model_describes_the_whole_source_while_holding_a_viewport() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
    let rows = Rows::new(SOURCE);
    rows.resize(Version::new(1), LENGTH);

    assert_eq!(
        u64::from(rows.n_items()),
        LENGTH,
        "the scrollbar must describe the source, not the cache"
    );
    assert_eq!(rows.held(), 0, "nothing is held before anything is asked for");
}

#[test]
fn realizing_a_row_answers_at_once_and_asks_for_the_rest() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
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

#[test]
fn a_delivered_window_replaces_the_placeholders_it_covers() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
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

#[test]
fn scrolling_a_large_source_holds_only_a_bounded_number_of_rows() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
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

#[test]
fn an_invalidated_band_returns_to_pending_without_disturbing_the_rest() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
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

#[test]
fn a_window_arriving_before_a_length_is_still_reachable() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
    let rows = Rows::new(SOURCE);
    let _ = rows.item(0);
    let requests = rows.drain();

    // Nothing was scheduled, because the model does not yet know the source has
    // any rows; a producer may still push a window first.
    assert!(requests.is_empty());
    rows.resize(Version::new(1), 128);
    let _ = rows.item(0);
    for request in rows.drain() {
        rows.deliver(&answer(&request));
    }
    assert!(!rows.is_pending(0));
}
