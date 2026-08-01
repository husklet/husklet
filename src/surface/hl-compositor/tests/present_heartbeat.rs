#[test]
fn heartbeat_reports_first_then_on_cadence_and_never_goes_quiet() {
    use hl_compositor::diagnostic::Heartbeat;
    use std::time::Duration;
    let mut hb: Heartbeat<&str> = Heartbeat::new(Duration::from_millis(40));
    // First occurrence speaks immediately — a reader must not wait an interval to learn it started.
    let first = hb.record("a").expect("first occurrence must report");
    assert_eq!((first.total, first.in_window), (1, 1));
    // Inside the interval it stays quiet.
    for _ in 0..50 {
        assert!(hb.record("a").is_none(), "reported inside the interval");
    }
    std::thread::sleep(Duration::from_millis(45));
    let beat = hb.record("a").expect("must report once the interval elapsed");
    assert_eq!(beat.total, 52);
    assert_eq!(beat.in_window, 51, "the window count is since the last report");
    // Unlike a power-of-ten latch, it keeps speaking however long it runs.
    for round in 0..3 {
        std::thread::sleep(Duration::from_millis(45));
        assert!(hb.record("a").is_some(), "went quiet on round {round}");
    }
    // Distinct keys are independent, which is what makes shown/offscreen separable per surface.
    assert!(hb.record("b").is_some(), "a new key must report immediately");
}
