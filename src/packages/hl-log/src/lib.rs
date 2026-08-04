//! Transferable structured logging and bounded diagnostics.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Severity ordered from most to least important.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// One structured field value without allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Value<'a> {
    Text(&'a str),
    Signed(i64),
    Unsigned(u64),
    Boolean(bool),
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(value) => formatter.write_str(value),
            Self::Signed(value) => value.fmt(formatter),
            Self::Unsigned(value) => value.fmt(formatter),
            Self::Boolean(value) => value.fmt(formatter),
        }
    }
}

/// One key/value field.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Field<'a> {
    pub key: &'a str,
    pub value: Value<'a>,
}

/// Synchronous borrowed diagnostic record.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Record<'a> {
    pub level: Level,
    pub target: &'a str,
    pub message: &'a str,
    pub fields: &'a [Field<'a>],
}

/// Sink failure kept independent from any logging backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SinkError {
    message: String,
}

impl SinkError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SinkError {}

/// Synchronous destination. Implementations must not retain borrowed records.
pub trait Sink: Send + Sync {
    fn write(&self, record: &Record<'_>) -> Result<(), SinkError>;
}

impl<T: Sink + ?Sized> Sink for Arc<T> {
    fn write(&self, record: &Record<'_>) -> Result<(), SinkError> {
        T::write(self, record)
    }
}

/// Inclusive severity threshold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Filter {
    maximum: Level,
}

impl Filter {
    #[must_use]
    pub const fn through(maximum: Level) -> Self {
        Self { maximum }
    }

    #[must_use]
    pub fn permits(self, level: Level) -> bool {
        level <= self.maximum
    }
}

/// Hard limits preventing diagnostics from becoming a memory/CPU attack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordLimits {
    pub target_bytes: usize,
    pub message_bytes: usize,
    pub fields: usize,
    pub field_key_bytes: usize,
    pub text_value_bytes: usize,
}

impl Default for RecordLimits {
    fn default() -> Self {
        Self {
            target_bytes: 128,
            message_bytes: 4096,
            fields: 32,
            field_key_bytes: 128,
            text_value_bytes: 4096,
        }
    }
}

/// Duplicate emission policy per stable record fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RatePolicy {
    pub interval: Duration,
    pub burst: u32,
}

impl RatePolicy {
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            interval: Duration::ZERO,
            burst: u32::MAX,
        }
    }
}

/// Injectable monotonic clock for deterministic rate-limit tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Process-local monotonic clock.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self { origin: Instant::now() }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

#[derive(Clone, Copy, Debug)]
struct RateState {
    window: Duration,
    emitted: u32,
    suppressed: u64,
}

/// Result distinguishes filtering from duplicate suppression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogOutcome {
    Written,
    Filtered,
    Suppressed { duplicates: u64 },
}

/// Logger rejection before a sink is called.
#[derive(Debug)]
pub enum LogError {
    OversizedRecord,
    Sink(SinkError),
}

impl fmt::Display for LogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OversizedRecord => formatter.write_str("log record exceeds limits"),
            Self::Sink(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LogError {}

/// Filtered, bounded and rate-limited logger.
pub struct Logger<S, C = SystemClock> {
    sink: S,
    clock: C,
    filter: Filter,
    limits: RecordLimits,
    rate: RatePolicy,
    rates: Mutex<HashMap<u64, RateState>>,
}

impl<S: Sink, C: Clock> Logger<S, C> {
    #[must_use]
    pub fn new(sink: S, clock: C, filter: Filter, limits: RecordLimits, rate: RatePolicy) -> Self {
        Self {
            sink,
            clock,
            filter,
            limits,
            rate,
            rates: Mutex::new(HashMap::new()),
        }
    }

    pub fn log(&self, record: &Record<'_>) -> Result<LogOutcome, LogError> {
        if !self.filter.permits(record.level) {
            return Ok(LogOutcome::Filtered);
        }
        if !self.limits.accepts(record) {
            return Err(LogError::OversizedRecord);
        }
        let duplicate = self.admit(record);
        if let Some(duplicates) = duplicate {
            return Ok(LogOutcome::Suppressed { duplicates });
        }
        self.sink.write(record).map_err(LogError::Sink)?;
        Ok(LogOutcome::Written)
    }

    fn admit(&self, record: &Record<'_>) -> Option<u64> {
        if self.rate == RatePolicy::unlimited() {
            return None;
        }
        let fingerprint = RecordFingerprint::of(record);
        let now = self.clock.now();
        let mut rates = self.rates.lock().unwrap_or_else(|error| error.into_inner());
        let state = rates.entry(fingerprint).or_insert(RateState {
            window: now,
            emitted: 0,
            suppressed: 0,
        });
        if now.saturating_sub(state.window) >= self.rate.interval {
            *state = RateState {
                window: now,
                emitted: 0,
                suppressed: 0,
            };
        }
        if state.emitted < self.rate.burst {
            state.emitted += 1;
            return None;
        }
        state.suppressed = state.suppressed.saturating_add(1);
        Some(state.suppressed)
    }
}

impl RecordLimits {
    fn accepts(self, record: &Record<'_>) -> bool {
        record.target.len() <= self.target_bytes
            && record.message.len() <= self.message_bytes
            && record.fields.len() <= self.fields
            && record.fields.iter().all(|field| {
                field.key.len() <= self.field_key_bytes
                    && !matches!(
                        field.value,
                        Value::Text(value) if value.len() > self.text_value_bytes
                    )
            })
    }
}

struct RecordFingerprint;

impl RecordFingerprint {
    fn of(record: &Record<'_>) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        record.hash(&mut hasher);
        hasher.finish()
    }
}

/// Owned test record captured by [`MemorySink`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedRecord {
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: Vec<(String, String)>,
}

/// Thread-safe in-memory sink for tests and embedding diagnostics.
#[derive(Debug, Default)]
pub struct MemorySink {
    records: Mutex<Vec<CapturedRecord>>,
}

impl MemorySink {
    #[must_use]
    pub fn records(&self) -> Vec<CapturedRecord> {
        self.records.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }
}

impl Sink for MemorySink {
    fn write(&self, record: &Record<'_>) -> Result<(), SinkError> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(CapturedRecord {
                level: record.level,
                target: record.target.to_owned(),
                message: record.message.to_owned(),
                fields: record
                    .fields
                    .iter()
                    .map(|field| (field.key.to_owned(), field.value.to_string()))
                    .collect(),
            });
        Ok(())
    }
}

/// Sends each record to every sink in insertion order.
#[derive(Default)]
pub struct Fanout {
    sinks: Vec<Arc<dyn Sink>>,
}

impl Fanout {
    #[must_use]
    pub const fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    pub fn push(&mut self, sink: Arc<dyn Sink>) {
        self.sinks.push(sink);
    }
}

impl Sink for Fanout {
    fn write(&self, record: &Record<'_>) -> Result<(), SinkError> {
        for sink in &self.sinks {
            sink.write(record)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct TestClock(AtomicU64);

    impl Clock for TestClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.0.load(Ordering::Acquire))
        }
    }

    fn record<'a>(fields: &'a [Field<'a>]) -> Record<'a> {
        Record {
            level: Level::Info,
            target: "test",
            message: "message",
            fields,
        }
    }

    #[test]
    fn filtering_skips_sink() {
        let sink = Arc::new(MemorySink::default());
        let logger = Logger::new(
            sink.clone(),
            TestClock::default(),
            Filter::through(Level::Warn),
            RecordLimits::default(),
            RatePolicy::unlimited(),
        );
        assert_eq!(logger.log(&record(&[])).unwrap(), LogOutcome::Filtered);
        assert!(sink.records().is_empty());
    }

    #[test]
    fn owned_fields() {
        let sink = Arc::new(MemorySink::default());
        let logger = Logger::new(
            sink.clone(),
            TestClock::default(),
            Filter::through(Level::Trace),
            RecordLimits::default(),
            RatePolicy::unlimited(),
        );
        let fields = [
            Field {
                key: "count",
                value: Value::Unsigned(7),
            },
            Field {
                key: "ready",
                value: Value::Boolean(true),
            },
        ];
        assert_eq!(logger.log(&record(&fields)).unwrap(), LogOutcome::Written);
        assert_eq!(
            sink.records()[0].fields,
            [("count".into(), "7".into()), ("ready".into(), "true".into())]
        );
    }

    #[test]
    fn record_preflight() {
        let sink = Arc::new(MemorySink::default());
        let logger = Logger::new(
            sink.clone(),
            TestClock::default(),
            Filter::through(Level::Trace),
            RecordLimits {
                message_bytes: 3,
                ..RecordLimits::default()
            },
            RatePolicy::unlimited(),
        );
        assert!(matches!(logger.log(&record(&[])), Err(LogError::OversizedRecord)));
        assert!(sink.records().is_empty());
    }

    #[test]
    fn burst_window_reset() {
        let sink = Arc::new(MemorySink::default());
        let clock = TestClock::default();
        let logger = Logger::new(
            sink.clone(),
            clock,
            Filter::through(Level::Trace),
            RecordLimits::default(),
            RatePolicy {
                interval: Duration::from_nanos(10),
                burst: 2,
            },
        );
        assert_eq!(logger.log(&record(&[])).unwrap(), LogOutcome::Written);
        assert_eq!(logger.log(&record(&[])).unwrap(), LogOutcome::Written);
        assert_eq!(
            logger.log(&record(&[])).unwrap(),
            LogOutcome::Suppressed { duplicates: 1 }
        );
        logger.clock.0.store(10, Ordering::Release);
        assert_eq!(logger.log(&record(&[])).unwrap(), LogOutcome::Written);
        assert_eq!(sink.records().len(), 3);
    }

    #[test]
    fn ordered_fanout() {
        let first = Arc::new(MemorySink::default());
        let second = Arc::new(MemorySink::default());
        let mut fanout = Fanout::new();
        fanout.push(first.clone());
        fanout.push(second.clone());
        fanout.write(&record(&[])).unwrap();
        assert_eq!(first.records().len(), 1);
        assert_eq!(second.records().len(), 1);
    }
}
