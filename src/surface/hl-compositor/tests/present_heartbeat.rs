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
    let beat = hb
        .record("a")
        .expect("must report once the interval elapsed");
    assert_eq!(beat.total, 52);
    assert_eq!(
        beat.in_window, 51,
        "the window count is since the last report"
    );
    // Unlike a power-of-ten latch, it keeps speaking however long it runs.
    for round in 0..3 {
        std::thread::sleep(Duration::from_millis(45));
        assert!(hb.record("a").is_some(), "went quiet on round {round}");
    }
    // Distinct keys are independent, which is what makes shown/offscreen separable per surface.
    assert!(
        hb.record("b").is_some(),
        "a new key must report immediately"
    );
}

/// The route change is what gets reported, not the route.
///
/// A surface presenting offscreen does so every frame, so reporting the state would be one line per
/// frame forever. What an operator needs is the transition: the first frame that took a route, and —
/// more importantly — whether the surface ever stopped taking it. An unreconciled window role is a
/// startup race that resolves; a missing main-thread handle never will; and the difference between a
/// race and a stuck surface is entirely whether a later `shown` line appears.
///
/// This pins the de-duplication rule the presenter uses, since the presenter itself is macOS-only and
/// cannot be constructed here.
#[test]
fn a_route_is_reported_on_change_and_not_repeated() {
    use std::collections::HashMap;
    let mut routes: HashMap<u32, &'static str> = HashMap::new();
    let mut reported: Vec<(u32, &'static str, Option<&'static str>)> = Vec::new();
    let mut observe = |sid: u32, route: &'static str| {
        if routes.get(&sid) != Some(&route) {
            let previous = routes.insert(sid, route);
            reported.push((sid, route, previous));
        }
    };

    // A surface races: offscreen for a while, then reconciles and shows.
    for _ in 0..500 {
        observe(1, "offscreen-unreconciled");
    }
    for _ in 0..500 {
        observe(1, "shown");
    }
    // A second surface is stuck offscreen for the whole run.
    for _ in 0..1000 {
        observe(2, "offscreen-headless");
    }

    assert_eq!(
        reported,
        vec![
            (1, "offscreen-unreconciled", None),
            (1, "shown", Some("offscreen-unreconciled")),
            (2, "offscreen-headless", None),
        ],
        "a thousand frames per surface must produce one line per route change, carrying the route it \
         came from so a reader can see the recovery",
    );
    // The distinguishing property: the racing surface has a `shown` line and the stuck one does not.
    assert!(reported
        .iter()
        .any(|(sid, route, _)| *sid == 1 && *route == "shown"));
    assert!(!reported
        .iter()
        .any(|(sid, route, _)| *sid == 2 && *route == "shown"));
}
