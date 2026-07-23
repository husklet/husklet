//! The ergonomic macros — and the compile-time debug/release policy.
//!
//! # Compile-time inclusion policy
//! Every level macro has TWO definitions selected by mutually-exclusive `#[cfg]`:
//! a real one and a no-op that expands to `{}`. Which one the compiler keeps decides
//! whether the call site produces any code at all.
//!
//! | macro                         | debug | release (default) | release + `release-verbose` | `disabled` |
//! |-------------------------------|-------|-------------------|-----------------------------|------------|
//! | `hl_error!`                   |  yes  |       yes         |            yes              |    no      |
//! | `hl_warn!`/`info`/`debug`/`trace` | yes |       NO        |            yes              |    no      |
//! | `hl_count!`/`hl_add!`/`hl_span!`  | yes |       NO        |            yes              |    no      |
//! | `hl_log!` (runtime level)     |  yes  |       yes         |            yes              |    no      |
//!
//! So in a normal `--release` build the verbose levels and all profiling **compile to
//! nothing** — the branches, the `format_args!`, the argument expressions: gone. Only
//! `hl_error!` survives, because you always want to be able to surface an error in
//! release. `hl_log!` also survives as the runtime-level escape hatch.
//!
//! In debug builds every macro is present but still fronted by the runtime gate, so a
//! debug build with the default configuration is also near-free (one relaxed load + branch).
//!
//! # Why `format_args!` lives INSIDE the `if`
//! The real body is `if enabled(..) { emit(.., format_args!(..)) }`. When the gate is
//! false the arguments are never formatted and never evaluated — no allocation, no
//! side effects. This is the runtime half of the zero-cost guarantee; the `#[cfg]`
//! no-op is the compile-time half.

// ---------------------------------------------------------------------------------
// Shared real body. Present whenever ANY logging is compiled in (i.e. not fully
// `disabled`). Both `hl_error!` (always) and the verbose macros (debug-only) route
// through it, so the gate + emit shape lives in exactly one place.
// ---------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))]
#[macro_export]
#[doc(hidden)]
macro_rules! __hl_do {
    ($tag:expr, $level:expr, $($arg:tt)+) => {{
        let tags = $crate::Tags::from($tag);
        if $crate::Logging::global().enabled(tags, $level) {
            $crate::emit(tags, $level, module_path!(), line!(), format_args!($($arg)+));
        }
    }};
}

#[cfg(feature = "disabled")]
#[macro_export]
#[doc(hidden)]
macro_rules! __hl_do {
    ($tag:expr, $level:expr, $($arg:tt)+) => {{
        if false {
            let _ = (&$tag, &$level, format_args!($($arg)+));
        }
    }};
}

// ---------------------------------------------------------------------------------
// hl_log! — core macro with a runtime `Level`. Always compiled unless fully disabled
// (its level is a runtime value, so it can't be cfg-stripped per-level).
// ---------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))]
#[macro_export]
macro_rules! hl_log {
    ($tag:expr, $level:expr, $($arg:tt)+) => {
        $crate::__hl_do!($tag, $level, $($arg)+)
    };
}

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! hl_log {
    ($tag:expr, $level:expr, $($arg:tt)+) => {
        $crate::__hl_do!($tag, $level, $($arg)+)
    };
}

// ---------------------------------------------------------------------------------
// hl_error! — the ONE level that survives release. Compiled unless fully disabled.
// ---------------------------------------------------------------------------------

#[cfg(not(feature = "disabled"))]
#[macro_export]
macro_rules! hl_error {
    ($tag:expr, $($arg:tt)+) => {
        $crate::__hl_do!($tag, $crate::Level::Error, $($arg)+)
    };
}

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! hl_error {
    ($tag:expr, $($arg:tt)+) => {
        $crate::__hl_do!($tag, $crate::Level::Error, $($arg)+)
    };
}

// ---------------------------------------------------------------------------------
// Verbose levels: warn / info / debug / trace.
// Compiled in only for debug builds (or release + `release-verbose`); otherwise the
// call site expands to `{}` and produces no code.
// ---------------------------------------------------------------------------------

/// True-cfg = "verbose logging is compiled in".
// (documented alias for the long predicate below)
#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_warn {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Warn, $($arg)+) };
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_warn {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_info {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Info, $($arg)+) };
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_info {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_debug {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Debug, $($arg)+) };
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_debug {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_trace {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Trace, $($arg)+) };
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_trace {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

// ---------------------------------------------------------------------------------
// Counters + timing spans. Profiling is a debug-time activity, so these follow the
// same compile policy as the verbose levels.
// ---------------------------------------------------------------------------------

#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_count {
    ($tag:expr, $name:expr) => {{
        if $crate::Profiling::global().enabled($crate::Tags::from($tag)) {
            $crate::Counters::global().add($name, 1);
        }
    }};
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_count {
    ($tag:expr, $name:expr) => {{
        if false {
            let _ = (&$tag, &$name);
        }
    }};
}

#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_add {
    ($tag:expr, $name:expr, $n:expr) => {{
        if $crate::Profiling::global().enabled($crate::Tags::from($tag)) {
            $crate::Counters::global().add($name, $n);
        }
    }};
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_add {
    ($tag:expr, $name:expr, $n:expr) => {{
        if false {
            let _ = (&$tag, &$name, &$n);
        }
    }};
}

/// Open a timing span. Bind the result: `let _s = hl_span!(tag::WGPU, "readback");`.
/// Records elapsed time on drop when profiling includes the tag; otherwise the
/// returned guard is inert. In release (without `release-verbose`) it is always inert.
#[cfg(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
))]
#[macro_export]
macro_rules! hl_span {
    ($tag:expr, $name:expr) => {{
        if $crate::Profiling::global().enabled($crate::Tags::from($tag)) {
            $crate::Timings::global().start($name)
        } else {
            $crate::Span::disabled()
        }
    }};
}
#[cfg(not(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
)))]
#[macro_export]
macro_rules! hl_span {
    ($tag:expr, $name:expr) => {{
        if false {
            let _ = (&$tag, &$name);
        }
        $crate::Span::disabled()
    }};
}
