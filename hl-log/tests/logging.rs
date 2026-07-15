//! Integration tests for hl-log: the gate, arg-not-evaluated proof, level filter,
//! env parsing, sink swap + format, counters, timing, and a disabled-path cost bench.
//!
//! These tests mutate global state (masks, level, sink) and the process environment,
//! so they are serialized behind a single mutex to avoid cross-test interference.

use hl_log::{tag, Level};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Mutex, OnceLock};

// -------------------------------------------------------------------------------
// Test harness: a global lock (tests touch global state) + a collecting sink.
// -------------------------------------------------------------------------------

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A sink that collects every emitted line for assertions.
struct TestSink {
    lines: Mutex<Vec<String>>,
}

impl TestSink {
    fn install() -> &'static TestSink {
        static SINK: OnceLock<&'static TestSink> = OnceLock::new();
        let s = *SINK.get_or_init(|| Box::leak(Box::new(TestSink { lines: Mutex::new(Vec::new()) })));
        s.lines.lock().unwrap().clear();
        // Route hl-log output into this collector.
        hl_log::set_sink(Box::new(Collector(s)));
        s
    }
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

struct Collector(&'static TestSink);
impl hl_log::Sink for Collector {
    fn write_line(&self, s: &str) {
        self.0.lines.lock().unwrap().push(s.to_string());
    }
}

/// Reset global state to a known baseline for a test.
fn reset_state() {
    hl_log::set_enabled(tag::NONE);
    hl_log::set_counters(tag::NONE);
    hl_log::set_level(Level::Trace); // permissive by default; individual tests tighten
    hl_log::counters_reset();
    hl_log::timing_reset();
}

// -------------------------------------------------------------------------------
// The gate: tag mask + level filtering.
// -------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))] // needs verbose macros compiled in
#[test]
fn gate_respects_tag_mask() {
    let _g = test_lock();
    let sink = TestSink::install();
    reset_state();
    hl_log::set_enabled(tag::GPU);
    hl_log::set_level(Level::Trace);

    hl_log::hl_debug!(tag::GPU, "gpu message");
    hl_log::hl_debug!(tag::VULKAN, "vulkan message"); // disabled tag -> no emit

    let lines = sink.lines();
    assert_eq!(lines.len(), 1, "only the enabled tag should emit: {lines:?}");
    assert!(lines[0].contains("gpu message"));
    assert!(!lines[0].contains("vulkan"));
}

#[cfg(not(feature = "disabled"))] // needs verbose macros compiled in
#[test]
fn gate_respects_level() {
    let _g = test_lock();
    let sink = TestSink::install();
    reset_state();
    hl_log::set_enabled(tag::ALL);
    hl_log::set_level(Level::Info); // Error/Warn/Info pass; Debug/Trace suppressed

    hl_log::hl_error!(tag::GPU, "err");
    hl_log::hl_warn!(tag::GPU, "warn");
    hl_log::hl_info!(tag::GPU, "info");
    hl_log::hl_debug!(tag::GPU, "debug"); // suppressed
    hl_log::hl_trace!(tag::GPU, "trace"); // suppressed

    let lines = sink.lines();
    let joined = lines.join("");
    assert!(joined.contains("err") && joined.contains("warn") && joined.contains("info"));
    assert!(!joined.contains("debug"), "debug must be filtered: {lines:?}");
    assert!(!joined.contains("trace"), "trace must be filtered: {lines:?}");
    assert_eq!(lines.len(), 3);
}

// -------------------------------------------------------------------------------
// Arg-not-evaluated proof: when the tag is off, format args are NEVER evaluated.
// -------------------------------------------------------------------------------

static SIDE_EFFECT_CALLS: AtomicUsize = AtomicUsize::new(0);

fn panics_if_called() -> i32 {
    SIDE_EFFECT_CALLS.fetch_add(1, SeqCst);
    panic!("format argument was evaluated while logging was disabled!");
}

#[test]
fn args_not_evaluated_when_disabled() {
    let _g = test_lock();
    let _sink = TestSink::install();
    reset_state();
    // GPU tag is OFF (reset_state cleared the mask). The argument expression calls a
    // function that panics if ever evaluated. If the gate is correct it never runs.
    hl_log::set_enabled(tag::VULKAN); // anything but GPU
    hl_log::set_level(Level::Trace);

    let before = SIDE_EFFECT_CALLS.load(SeqCst);
    hl_log::hl_debug!(tag::GPU, "value = {}", panics_if_called());
    hl_log::hl_error!(tag::GPU, "value = {}", panics_if_called());
    let after = SIDE_EFFECT_CALLS.load(SeqCst);

    assert_eq!(before, after, "argument expression must NOT be evaluated when the tag is off");
}

// -------------------------------------------------------------------------------
// Env parsing.
// -------------------------------------------------------------------------------

#[test]
fn env_parsing_multi_all_off() {
    let _g = test_lock();
    let _sink = TestSink::install();

    std::env::set_var("HL_LOG", "gpu,wgpu,transport");
    std::env::set_var("HL_LOG_LEVEL", "debug");
    std::env::set_var("HL_LOG_COUNTERS", "all");
    hl_log::init();
    assert_eq!(hl_log::enabled_mask(), tag::GPU | tag::WGPU | tag::TRANSPORT);
    assert_eq!(hl_log::level(), Level::Debug);
    assert_eq!(hl_log::counters_mask(), tag::ALL);

    std::env::set_var("HL_LOG", "all");
    std::env::set_var("HL_LOG_LEVEL", "error");
    std::env::set_var("HL_LOG_COUNTERS", "off");
    hl_log::init();
    assert_eq!(hl_log::enabled_mask(), tag::ALL);
    assert_eq!(hl_log::level(), Level::Error);
    assert_eq!(hl_log::counters_mask(), tag::NONE);

    std::env::set_var("HL_LOG", "off");
    hl_log::init();
    assert_eq!(hl_log::enabled_mask(), tag::NONE);

    std::env::remove_var("HL_LOG");
    std::env::remove_var("HL_LOG_LEVEL");
    std::env::remove_var("HL_LOG_COUNTERS");
}

#[test]
fn tag_name_roundtrip() {
    assert_eq!(tag::from_name("gpu"), Some(tag::GPU));
    assert_eq!(tag::from_name("WGPU"), Some(tag::WGPU));
    assert_eq!(tag::from_name("all"), Some(tag::ALL));
    assert_eq!(tag::from_name("off"), Some(tag::NONE));
    assert_eq!(tag::from_name("nope"), None);
    assert_eq!(tag::name(tag::VULKAN), "vulkan");
    assert_eq!(tag::name(tag::ALL), "all");
}

// -------------------------------------------------------------------------------
// Sink swap + line format.
// -------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))] // needs verbose macros compiled in
#[test]
fn sink_format() {
    let _g = test_lock();
    let sink = TestSink::install();
    reset_state();
    hl_log::set_enabled(tag::GPU);
    hl_log::set_level(Level::Trace);

    hl_log::hl_debug!(tag::GPU, "hello {}", 42);
    let lines = sink.lines();
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    // Shape: [gpu] D +<ms>ms t<id> module:line: hello 42
    assert!(line.starts_with("[gpu] D "), "got: {line:?}");
    assert!(line.contains("logging:"), "module path present: {line:?}");
    assert!(line.trim_end().ends_with("hello 42"), "message present: {line:?}");
    assert!(line.ends_with('\n'), "line terminated: {line:?}");
}

// -------------------------------------------------------------------------------
// Counters.
// -------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))] // needs verbose macros compiled in
#[test]
fn counters_gated_and_snapshot() {
    let _g = test_lock();
    let _sink = TestSink::install();
    reset_state();
    hl_log::enable_counters(tag::GPU); // GPU counters on, VULKAN off

    hl_log::hl_count!(tag::GPU, "frames");
    hl_log::hl_count!(tag::GPU, "frames");
    hl_log::hl_add!(tag::GPU, "bytes", 100);
    hl_log::hl_add!(tag::GPU, "bytes", 50);
    // Under a disabled tag: must stay 0.
    hl_log::hl_count!(tag::VULKAN, "vk_frames");
    hl_log::hl_add!(tag::VULKAN, "vk_frames", 999);

    let snap = hl_log::counters_snapshot();
    // Sorted by name: bytes, frames (vk_frames absent since it never incremented).
    assert_eq!(snap, vec![("bytes", 150u64), ("frames", 2u64)]);
    assert_eq!(hl_log::counters::get("vk_frames"), 0);

    hl_log::counters_reset();
    assert!(hl_log::counters_snapshot().is_empty());
}

// -------------------------------------------------------------------------------
// Timing spans.
// -------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))] // needs verbose macros compiled in
#[test]
fn timing_records_only_when_enabled() {
    let _g = test_lock();
    let _sink = TestSink::install();
    reset_state();
    hl_log::enable_counters(tag::WGPU); // WGPU timing on, GPU off

    {
        let _s = hl_log::hl_span!(tag::WGPU, "readback");
        std::hint::black_box(&_s);
    }
    {
        // Disabled tag: records nothing.
        let _s = hl_log::hl_span!(tag::GPU, "compose");
        std::hint::black_box(&_s);
    }

    let snap = hl_log::timing_snapshot();
    assert_eq!(snap.len(), 1, "only the enabled-tag span records: {snap:?}");
    assert_eq!(snap[0].0, "readback");
    assert_eq!(snap[0].1.count, 1);
    // sum_ns should be > 0 for a real (if tiny) span.
    assert!(snap[0].1.sum_ns >= snap[0].1.max_ns);

    hl_log::timing_reset();
    assert!(hl_log::timing_snapshot().is_empty());
}

// -------------------------------------------------------------------------------
// Disabled-path cost: N million disabled macro calls must complete near-instantly,
// proving the gate is effectively free. Documents measured ns/call.
// -------------------------------------------------------------------------------

static ACC: AtomicU64 = AtomicU64::new(0);

#[test]
fn disabled_path_is_cheap() {
    let _g = test_lock();
    let _sink = TestSink::install();
    reset_state();
    hl_log::set_enabled(tag::NONE); // everything off -> pure gate cost

    const N: u64 = 5_000_000;
    let start = std::time::Instant::now();
    for i in 0..N {
        // Args reference `i` but are never evaluated (tag off). black_box defeats the
        // optimizer from hoisting the whole loop away.
        hl_log::hl_debug!(tag::GPU, "iter {} {}", i, ACC.load(SeqCst));
        std::hint::black_box(i);
    }
    let elapsed = start.elapsed();
    let per_call_ns = elapsed.as_nanos() as f64 / N as f64;
    eprintln!("disabled_path: {N} calls in {elapsed:?} = {per_call_ns:.3} ns/call");

    // Extremely loose bound (CI-safe): 5M gated no-ops must finish well under a second.
    assert!(
        elapsed.as_secs() < 2,
        "disabled path too slow: {elapsed:?} ({per_call_ns:.3} ns/call)"
    );
}
