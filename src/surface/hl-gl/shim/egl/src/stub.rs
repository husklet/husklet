//! Default-stub bookkeeping for the generated `egl*`/`gl*` long tail.
//!
//! [`hit`] gives every generated default stub a once-per-name "unimplemented entry point" trace under
//! `HL_SHIM_DEBUG`; [`unsupported`] is the truthful-failure hook a hand-written body calls when it
//! reaches an operation the modeled GL/IR subset cannot represent (so it records an accurate GL/EGL error
//! instead of a false success). Mirrors `hl-cuda/shim/cuda/src/stub.rs`.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

fn seen() -> &'static Mutex<HashSet<&'static str>> {
    static S: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

fn queries() -> &'static Mutex<HashSet<(QueryKind, u32)>> {
    static QUERIES: OnceLock<Mutex<HashSet<(QueryKind, u32)>>> = OnceLock::new();
    QUERIES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn context_attribute_failures() -> &'static Mutex<HashSet<String>> {
    static FAILURES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn uploads() -> &'static Mutex<UploadStats> {
    static UPLOADS: OnceLock<Mutex<UploadStats>> = OnceLock::new();
    UPLOADS.get_or_init(|| Mutex::new(UploadStats::default()))
}

#[derive(Default)]
struct UploadStats {
    total: u64,
    pairs: Vec<UploadPair>,
    overflow: u64,
}

struct UploadPair {
    format: u32,
    type_: u32,
    converted: u64,
    rejected: u64,
    null: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum UploadOutcome {
    Converted,
    Null,
    Rejected,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum QueryKind {
    Boolean,
    Float,
    Integer,
    String,
}

impl QueryKind {
    fn name(self) -> &'static str {
        match self {
            Self::Boolean => "glGetBooleanv",
            Self::Float => "glGetFloatv",
            Self::Integer => "glGetIntegerv",
            Self::String => "glGetString",
        }
    }
}

/// Called by every generated stub. First hit of each name (when `HL_SHIM_DEBUG` is set) logs; the rest
/// are silent. Cheap and thread-safe.
pub struct Diagnostics;
impl Diagnostics {
    fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var_os("HL_SHIM_DEBUG").is_some())
    }

    #[inline]
    pub fn hit(name: &'static str) {
        if !Self::enabled() {
            return;
        }
        if let Ok(mut s) = seen().lock() {
            if s.insert(name) {
                eprintln!("[hl-gl-shim] unimplemented entry point: {name} (default stub)");
            }
        }
    }

    /// Report an unsupported GL/EGL operation the hand-written body could not honestly execute. Once-logs it
    /// under `HL_SHIM_DEBUG`. The caller records the accurate GL/EGL error itself.
    pub fn unsupported(cmd: &'static str, detail: &str) {
        if Self::enabled() {
            if let Ok(mut s) = seen().lock() {
                if s.insert(cmd) {
                    eprintln!("[hl-gl-shim] unsupported GL/EGL operation: {cmd} ({detail})");
                }
            }
        }
    }

    /// Once-log a successfully implemented entry point while diagnosing a real loader. This is intentionally
    /// behind the same explicit debug gate as stub reporting and is silent in normal guest processes.
    pub fn trace(cmd: &'static str, detail: &str) {
        if Self::enabled() {
            if let Ok(mut s) = seen().lock() {
                if s.insert(cmd) {
                    eprintln!("[hl-gl-shim] {cmd}: {detail}");
                }
            }
        }
    }

    /// Once-log each distinct GL query parameter and its modeled result. Capability discovery performs the
    /// same queries frequently, so keying by `(kind, pname)` preserves the complete negotiation without
    /// flooding the guest's stderr on every frame.
    pub fn query(kind: QueryKind, pname: u32, values: &str) {
        if !Self::enabled() {
            return;
        }
        if let Ok(mut seen) = queries().lock() {
            if seen.insert((kind, pname)) {
                eprintln!(
                    "[hl-gl-shim] {} pname=0x{pname:04x} values={values}",
                    kind.name()
                );
            }
        }
    }

    /// Report each distinct rejected EGL context attribute list once. Context creation can be retried
    /// hundreds of times by Chromium, so failures are deduplicated and capped while retaining the raw
    /// contract needed to diagnose an unsupported standard attribute.
    pub fn context_attributes(reason: &str, pairs: &[(i32, i32)]) {
        if !Self::enabled() {
            return;
        }
        let raw = pairs
            .iter()
            .map(|(attribute, value)| format!("0x{attribute:04x}=0x{value:04x}"))
            .collect::<Vec<_>>()
            .join(",");
        let finding = format!("reason={reason} attrs=[{raw}]");
        if let Ok(mut failures) = context_attribute_failures().lock() {
            if failures.len() < 16 && failures.insert(finding.clone()) {
                eprintln!("[hl-gl-shim] egl_context_attributes rejected {finding}");
            }
        }
    }

    /// Aggregate texture-upload formats under the explicit shim debug gate. The table retains at most 32
    /// distinct `(format,type)` pairs and prints only when the total call count reaches a power of two, so a
    /// long-running browser yields representative format/rejection evidence without a per-upload log flood.
    pub fn upload(format: u32, type_: u32, outcome: UploadOutcome) {
        if !Self::enabled() {
            return;
        }
        let Ok(mut stats) = uploads().lock() else {
            return;
        };
        stats.total = stats.total.saturating_add(1);
        if let Some(pair) = stats
            .pairs
            .iter_mut()
            .find(|pair| pair.format == format && pair.type_ == type_)
        {
            match outcome {
                UploadOutcome::Converted => pair.converted = pair.converted.saturating_add(1),
                UploadOutcome::Null => pair.null = pair.null.saturating_add(1),
                UploadOutcome::Rejected => pair.rejected = pair.rejected.saturating_add(1),
            }
        } else if stats.pairs.len() < 32 {
            stats.pairs.push(UploadPair {
                format,
                type_,
                converted: u64::from(outcome == UploadOutcome::Converted),
                rejected: u64::from(outcome == UploadOutcome::Rejected),
                null: u64::from(outcome == UploadOutcome::Null),
            });
        } else {
            stats.overflow = stats.overflow.saturating_add(1);
        }
        if stats.total.is_power_of_two() {
            let pairs = stats
                .pairs
                .iter()
                .map(|pair| {
                    format!(
                        "{:#x}/{:#x}:ok={},reject={},null={}",
                        pair.format, pair.type_, pair.converted, pair.rejected, pair.null
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!(
                "[hl-gl-shim] texture uploads total={} overflow={} {pairs}",
                stats.total, stats.overflow
            );
        }
    }
}

pub const HIT: fn(&'static str) = Diagnostics::hit;
pub const UNSUPPORTED: fn(&'static str, &str) = Diagnostics::unsupported;
pub const TRACE: fn(&'static str, &str) = Diagnostics::trace;
pub use HIT as hit;
pub use TRACE as trace;
pub use UNSUPPORTED as unsupported;
