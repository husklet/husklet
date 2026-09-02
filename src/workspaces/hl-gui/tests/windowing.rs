//! Windowing behaviour of the row cache, proven against a synthetic producer.
//!
//! Everything here runs headless and deterministically: time is a parameter,
//! not a clock. These are the guarantees a renderer relies on — a lookup never
//! blocks, a slow producer never queues unbounded work, and a superseded answer
//! never reaches the screen.

use hl_gui::{Cell, Lookup, RequestId, Row, RowCache, RowRange, RowRequest, RowWindow, Sort, SourceId, Version};

const SOURCE: SourceId = SourceId::new(1);

/// A producer that answers only when told to, so slowness is expressible.
struct Producer {
    version: Version,
    answered: Vec<RequestId>,
}

impl Producer {
    fn new() -> Self {
        Self {
            version: Version::new(1),
            answered: Vec::new(),
        }
    }

    fn answer(&mut self, request: &RowRequest) -> RowWindow {
        self.answered.push(request.id);
        let rows = (0..request.range.count)
            .map(|offset| {
                let index = request.range.start + u64::from(offset);
                Row::new(index, [Cell::text(format!("row {index}"))])
            })
            .collect();
        RowWindow {
            source: request.source,
            version: self.version,
            request: request.id,
            range: request.range,
            rows,
        }
    }
}

fn opened(rows: u64) -> (RowCache, Producer) {
    let mut cache = RowCache::new(SOURCE);
    let producer = Producer::new();
    cache.resize(producer.version, rows);
    (cache, producer)
}

/// Scrolls to `index` and settles every resulting request.
fn settle(cache: &mut RowCache, producer: &mut Producer, index: u64, now: u64) {
    let requests = cache.observe(RowRange::new(index, 40), now);
    for request in &requests {
        let window = producer.answer(request);
        cache.deliver(&window);
    }
}

#[test]
fn a_miss_answers_immediately_and_schedules_an_aligned_request() {
    let (mut cache, _) = opened(100_000);

    assert_eq!(cache.row(500), Lookup::Pending, "a lookup must never block");

    let requests = cache.observe(RowRange::new(500, 40), 0);
    let landing = requests
        .iter()
        .find(|request| request.range.contains(500))
        .expect("the viewport block is requested");
    assert_eq!(landing.range.start, 384, "requests are block aligned");
    assert_eq!(landing.range.count, RowRange::BLOCK);
}

#[test]
fn a_delivered_window_becomes_readable() {
    let (mut cache, mut producer) = opened(100_000);
    settle(&mut cache, &mut producer, 500, 0);

    match cache.row(500) {
        Lookup::Ready(row) => assert_eq!(row.cells, vec![Cell::text("row 500")]),
        other => panic!("expected a cached row, got {other:?}"),
    }
    assert_eq!(cache.in_flight(), 0);
}

#[test]
fn a_response_cannot_smuggle_rows_beyond_the_requested_window() {
    let (mut cache, mut producer) = opened(100_000);
    let request = cache.observe(RowRange::new(0, 40), 0).remove(0);
    let outstanding = cache.in_flight();
    let mut oversized = producer.answer(&request);
    oversized.rows.push(Row::new(
        u64::from(request.range.count),
        [Cell::text("outside request")],
    ));

    assert!(!cache.deliver(&oversized), "the whole oversized answer is refused");
    assert_eq!(cache.row(0), Lookup::Pending, "no prefix of the invalid answer lands");
    assert_eq!(
        cache.in_flight(),
        outstanding,
        "the valid outstanding request remains recoverable"
    );

    let valid = producer.answer(&request);
    assert!(cache.deliver(&valid));
    assert!(matches!(cache.row(0), Lookup::Ready(_)));
}

#[test]
fn a_long_scroll_requests_the_landing_block_not_every_block_crossed() {
    let (mut cache, mut producer) = opened(100_000);
    settle(&mut cache, &mut producer, 900, 0);

    let requests = cache.observe(RowRange::new(40_000, 40), 10);

    assert!(
        requests.len() <= 3,
        "landing plus prefetch only, got {} requests",
        requests.len()
    );
    assert!(
        requests.iter().any(|request| request.range.contains(40_000)),
        "the block the user landed on must be requested"
    );
    assert!(
        requests.iter().all(|request| request.range.start >= 39_000),
        "no request for blocks merely scrolled past"
    );
}

#[test]
fn outstanding_requests_are_capped() {
    let (mut cache, _) = opened(100_000);

    // A viewport far larger than the cap could ask for, with nothing answered.
    let requests = cache.observe(RowRange::new(0, 4_000), 0);

    assert_eq!(requests.len(), RowCache::IN_FLIGHT_LIMIT);
    assert_eq!(cache.in_flight(), RowCache::IN_FLIGHT_LIMIT);

    let more = cache.observe(RowRange::new(0, 4_000), 1);
    assert!(more.is_empty(), "the cap holds across observations");
}

#[test]
fn a_late_answer_to_a_superseded_request_is_dropped() {
    let (mut cache, mut producer) = opened(100_000);
    let requests = cache.observe(RowRange::new(500, 40), 0);
    let landing = requests
        .iter()
        .find(|request| request.range.contains(500))
        .expect("viewport block")
        .clone();

    // The request times out and is retried, so the original id is no longer current.
    let _ = cache.expire(RowCache::HARD_DEADLINE);

    let stale = producer.answer(&landing);
    assert!(!cache.deliver(&stale), "a cancelled request must not land");
    assert_eq!(cache.row(500), Lookup::Pending);
}

#[test]
fn a_window_from_a_superseded_generation_is_dropped() {
    let (mut cache, mut producer) = opened(100_000);
    let requests = cache.observe(RowRange::new(0, 40), 0);
    let request = requests.first().expect("a request").clone();
    let window = producer.answer(&request);

    cache.resize(Version::new(9), 100_000);

    assert!(
        !cache.deliver(&window),
        "rows from an older generation must not appear beside current ones"
    );
    assert!(cache.is_empty());
}

#[test]
fn a_slow_block_is_reported_before_it_is_abandoned() {
    let (mut cache, _) = opened(100_000);
    cache.observe(RowRange::new(0, 40), 0);

    cache.observe(RowRange::new(0, 40), RowCache::SOFT_DEADLINE - 1);
    assert!(!cache.is_slow(0), "still within the soft deadline");

    cache.observe(RowRange::new(0, 40), RowCache::SOFT_DEADLINE);
    assert!(cache.is_slow(0), "past the soft deadline it reads as slow");
    assert_eq!(cache.row(0), Lookup::Pending, "slow is not yet failed");
}

#[test]
fn a_timed_out_block_is_retried_once_then_declared_unavailable() {
    let (mut cache, _) = opened(100_000);
    cache.observe(RowRange::new(0, 40), 0);

    let retried = cache.expire(RowCache::HARD_DEADLINE);
    assert!(
        retried.iter().any(|request| request.range.contains(0)),
        "the first timeout retries"
    );
    assert_eq!(cache.row(0), Lookup::Pending);

    let again = cache.expire(RowCache::HARD_DEADLINE * 2);
    assert!(
        !again.iter().any(|request| request.range.contains(0)),
        "the second timeout gives up rather than retrying forever"
    );
    assert_eq!(
        cache.row(0),
        Lookup::Unavailable,
        "the renderer is told the rows are unavailable, not left waiting"
    );
}

#[test]
fn a_ranged_invalidation_evicts_only_that_band() {
    let (mut cache, mut producer) = opened(100_000);
    settle(&mut cache, &mut producer, 0, 0);
    settle(&mut cache, &mut producer, 512, 1);
    assert!(matches!(cache.row(0), Lookup::Ready(_)));
    assert!(matches!(cache.row(512), Lookup::Ready(_)));

    cache.invalidate(Version::new(1), Some(RowRange::new(512, 8)));

    assert!(
        matches!(cache.row(0), Lookup::Ready(_)),
        "an unrelated band survives, so one changed row does not blank the table"
    );
    assert_eq!(cache.row(512), Lookup::Pending);
}

#[test]
fn a_full_invalidation_clears_every_cached_row() {
    let (mut cache, mut producer) = opened(100_000);
    settle(&mut cache, &mut producer, 0, 0);
    assert!(!cache.is_empty());

    cache.invalidate(Version::new(2), None);

    assert!(cache.is_empty());
    assert_eq!(cache.row(0), Lookup::Pending);
}

#[test]
fn a_slow_producer_cannot_grow_the_cache_without_bound() {
    let (mut cache, mut producer) = opened(1_000_000);

    // Scroll the whole source, answering everything, which is the worst case
    // for retention: nothing is ever refused and nothing times out.
    let mut now = 0;
    let mut index = 0;
    while index < 200_000 {
        settle(&mut cache, &mut producer, index, now);
        index += 256;
        now += 1;
    }

    assert!(
        cache.len() <= RowCache::CAPACITY,
        "cache held {} rows, above its {} bound",
        cache.len(),
        RowCache::CAPACITY
    );
    assert!(
        matches!(cache.row(index - 256), Lookup::Ready(_)),
        "eviction must never drop what is on screen"
    );
}

#[test]
fn rows_past_the_end_are_absent_rather_than_pending() {
    let (mut cache, mut producer) = opened(50);
    settle(&mut cache, &mut producer, 0, 0);

    assert!(matches!(cache.row(49), Lookup::Ready(_)));
    assert_eq!(
        cache.row(50),
        Lookup::Absent,
        "past the end is a known answer, not a pending one"
    );
}

#[test]
fn a_source_shorter_than_a_block_is_not_over_requested() {
    let (mut cache, _) = opened(10);

    let requests = cache.observe(RowRange::new(0, 10), 0);

    assert_eq!(requests.len(), 1, "one block covers the whole source");
    assert_eq!(requests[0].range.start, 0);
}

#[test]
fn sorting_invalidates_every_cached_row_and_carries_the_intent() {
    let (mut cache, mut producer) = opened(100_000);
    settle(&mut cache, &mut producer, 0, 0);
    let before = cache.version();

    cache.sort(Some(Sort {
        column: "name".into(),
        descending: true,
    }));

    assert!(cache.is_empty(), "an order change invalidates every row");
    assert!(cache.version() > before, "and starts a new generation");

    let requests = cache.observe(RowRange::new(0, 40), 2);
    let sort = requests[0].sort.as_ref().expect("sort travels to producer");
    assert_eq!(sort.column, "name");
    assert!(sort.descending);
}

#[test]
fn filtering_invalidates_the_row_count_as_well_as_the_rows() {
    let (mut cache, mut producer) = opened(100_000);
    settle(&mut cache, &mut producer, 0, 0);

    cache.filter(Some("alpine".into()));

    assert!(cache.is_empty());
    assert_eq!(
        cache.length(),
        None,
        "the old count describes the unfiltered source and must not be reused"
    );
    let requests = cache.observe(RowRange::new(0, 40), 3);
    assert_eq!(requests[0].filter.as_deref(), Some("alpine"));
}

#[test]
fn a_window_for_another_source_is_refused() {
    let (mut cache, mut producer) = opened(100_000);
    let requests = cache.observe(RowRange::new(0, 40), 0);
    let mut window = producer.answer(&requests[0]);
    window.source = SourceId::new(99);

    assert!(!cache.deliver(&window));
}
