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
#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
#[macro_export]
macro_rules! hl_warn {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Warn, $($arg)+) };
}
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
#[macro_export]
macro_rules! hl_warn {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
#[macro_export]
macro_rules! hl_info {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Info, $($arg)+) };
}
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
#[macro_export]
macro_rules! hl_info {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
#[macro_export]
macro_rules! hl_debug {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Debug, $($arg)+) };
}
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
#[macro_export]
macro_rules! hl_debug {
    ($tag:expr, $($arg:tt)+) => {{
        if false { let _ = (&$tag, format_args!($($arg)+)); }
    }};
}

#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
#[macro_export]
macro_rules! hl_trace {
    ($tag:expr, $($arg:tt)+) => { $crate::__hl_do!($tag, $crate::Level::Trace, $($arg)+) };
}
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
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

#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
#[macro_export]
macro_rules! hl_count {
    ($tag:expr, $name:expr) => {{
        if $crate::Profiling::global().enabled($crate::Tags::from($tag)) {
            $crate::Counters::global().add($name, 1);
        }
    }};
}
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
#[macro_export]
macro_rules! hl_count {
    ($tag:expr, $name:expr) => {{
        if false {
            let _ = (&$tag, &$name);
        }
    }};
}

#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
#[macro_export]
macro_rules! hl_add {
    ($tag:expr, $name:expr, $n:expr) => {{
        if $crate::Profiling::global().enabled($crate::Tags::from($tag)) {
            $crate::Counters::global().add($name, $n);
        }
    }};
}
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
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
#[cfg(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose")))]
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
#[cfg(not(all(not(feature = "disabled"), any(debug_assertions, feature = "release-verbose"))))]
#[macro_export]
macro_rules! hl_span {
    ($tag:expr, $name:expr) => {{
        if false {
            let _ = (&$tag, &$name);
        }
        $crate::Span::disabled()
    }};
}

// ---------------------------------------------------------------------------------
// Structured events. See `event.rs` for the record shape and the reasoning.
// ---------------------------------------------------------------------------------

/// Accumulate `key = value` fields into an array of `(&str, Value)`.
///
/// A token-muncher rather than one repetition, because `$($sigil:tt)? $value:expr` is ambiguous to the
/// macro parser — it cannot tell a `tt` that might be absent from the start of an `expr`. One arm per
/// form, each with and without a trailing rest, is the shape that compiles.
#[macro_export]
#[doc(hidden)]
macro_rules! __hl_fields {
    (@acc [$($acc:expr),*]) => { [$($acc),*] };

    (@acc [$($acc:expr),*] $key:ident = % $value:expr) => {
        $crate::__hl_fields!(@acc [$($acc,)*
            (stringify!($key), $crate::Value::Text(format!("{}", $value)))])
    };
    (@acc [$($acc:expr),*] $key:ident = % $value:expr, $($rest:tt)*) => {
        $crate::__hl_fields!(@acc [$($acc,)*
            (stringify!($key), $crate::Value::Text(format!("{}", $value)))] $($rest)*)
    };

    (@acc [$($acc:expr),*] $key:ident = ? $value:expr) => {
        $crate::__hl_fields!(@acc [$($acc,)*
            (stringify!($key), $crate::Value::Text(format!("{:?}", $value)))])
    };
    (@acc [$($acc:expr),*] $key:ident = ? $value:expr, $($rest:tt)*) => {
        $crate::__hl_fields!(@acc [$($acc,)*
            (stringify!($key), $crate::Value::Text(format!("{:?}", $value)))] $($rest)*)
    };

    (@acc [$($acc:expr),*] $key:ident = $value:expr) => {
        $crate::__hl_fields!(@acc [$($acc,)* (stringify!($key), $crate::Value::from($value))])
    };
    (@acc [$($acc:expr),*] $key:ident = $value:expr, $($rest:tt)*) => {
        $crate::__hl_fields!(@acc [$($acc,)* (stringify!($key), $crate::Value::from($value))] $($rest)*)
    };

    ($($fields:tt)*) => { $crate::__hl_fields!(@acc [] $($fields)*) };
}

/// Emit a structured event: a stable name plus typed fields, machine-readable on the events channel.
///
/// ```
/// # use hl_log::{hl_event, tag, Level};
/// # let (id, bytes) = (7u32, 4096usize);
/// hl_event!(tag::GPU, Level::Debug, "frame.submitted", id = id, bytes = bytes);
/// ```
///
/// Gated exactly like every other macro — the same tag mask, the same level — and subject to the same
/// compile-time policy, so a verbose event vanishes in release and a closed tag never evaluates a field.
/// A record that is refused by the gate costs one relaxed load and a branch.
///
/// For an outcome the caller is entitled to regardless of which tags an operator happened to open, use
/// [`hl_verdict!`] instead.
#[cfg(not(feature = "disabled"))]
#[macro_export]
macro_rules! hl_event {
    ($tag:expr, $level:expr, $event:expr) => {
        $crate::hl_event!($tag, $level, $event,)
    };
    ($tag:expr, $level:expr, $event:expr, $($fields:tt)*) => {{
        let tags = $crate::Tags::from($tag);
        let level = $level;
        if $crate::Logging::global().enabled(tags, level) {
            $crate::emit_event(
                tags,
                level,
                $event,
                module_path!(),
                line!(),
                &$crate::__hl_fields!($($fields)*),
            );
        }
    }};
}

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! hl_event {
    ($tag:expr, $level:expr, $event:expr $(, $($fields:tt)*)?) => {{
        if false {
            let _ = (&$tag, &$level, &$event);
        }
    }};
}

/// Emit a VERDICT: a record that work the caller asked for did not happen.
///
/// A verdict is not narration and is deliberately **not subject to the tag mask**. Every other macro in
/// this crate is a subscription — an operator opens a tag to hear a subsystem talk about itself, and a
/// closed tag means "I am not listening to this". That is the right default for chatter and the wrong
/// one for a refusal, because the operator who did not know to open `wgpu` is exactly the person whose
/// frame was dropped. Measured: a run reported its bundle emitted no presentation heartbeat when the
/// diagnostic was at error level and firing — the tag was simply never opened, and the absence read as
/// a property of the subject.
///
/// The class is deliberately small, and the test is one question: **did the caller ask for something
/// that did not happen, where the caller cannot otherwise tell?** That admits a refused frame, a dropped
/// command, a resource that could not be created, a lost device or share group, and a capability
/// advertised but not honoured. It excludes successes, state transitions, timings, counts, retries that
/// later succeed, and anything the caller learns from a returned error — all of which stay behind their
/// tags. If a class of event would be useful to have on by default, that is not enough: the question is
/// whether omitting it makes a failure unattributable.
///
/// ```
/// # use hl_log::{hl_verdict, tag};
/// # let (id, reason) = (7u32, "no matching format");
/// hl_verdict!(tag::WGPU, "frame.refused", id = id, reason = %reason);
/// ```
///
/// Emits twice on purpose: the human sentence to the normal log at error level, so a person reading
/// stderr sees it without configuring anything, and the structured record to the events channel for
/// whoever is consuming one. It survives a release build for the same reason `hl_error!` does.
#[cfg(not(feature = "disabled"))]
#[macro_export]
macro_rules! hl_verdict {
    ($tag:expr, $event:expr) => {
        $crate::hl_verdict!($tag, $event,)
    };
    ($tag:expr, $event:expr, $($rest:tt)*) => {{
        let __hl_tag = $crate::Tags::from($tag);
        let __hl_event = $event;
        $crate::__hl_verdict_go!(__hl_tag, __hl_event, [] $($rest)*)
    }};
}

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! hl_verdict {
    ($tag:expr, $event:expr $(, $key:ident = $($sigil:tt)? $value:expr)* $(,)?) => {{
        if false {
            let _ = (&$tag, &$event $(, &$value)*);
        }
    }};
}

/// The verdict muncher.
///
/// Separate from [`__hl_fields`] because a verdict may carry a human sentence after `;`, and a `tt`
/// repetition that could swallow the `;` is ambiguous to the macro parser. The separator therefore has
/// to be matched explicitly everywhere a field can end, which is why there are three arms per field
/// form rather than one.
#[macro_export]
#[doc(hidden)]
macro_rules! __hl_verdict_go {
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = % $value:expr, $($rest:tt)*) => {
        $crate::__hl_verdict_go!($tag, $event, [$($acc,)* (stringify!($key), $crate::Value::Text(format!("{}", $value)))] $($rest)*)
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = % $value:expr ; $($human:tt)+) => {
        $crate::emit_verdict_with(
            $tag, $event, module_path!(), line!(),
            &[$($acc,)* (stringify!($key), $crate::Value::Text(format!("{}", $value)))],
            format_args!($($human)+),
        )
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = % $value:expr) => {
        $crate::emit_verdict(
            $tag, $event, module_path!(), line!(),
            &[$($acc,)* (stringify!($key), $crate::Value::Text(format!("{}", $value)))],
        )
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = ? $value:expr, $($rest:tt)*) => {
        $crate::__hl_verdict_go!($tag, $event, [$($acc,)* (stringify!($key), $crate::Value::Text(format!("{:?}", $value)))] $($rest)*)
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = ? $value:expr ; $($human:tt)+) => {
        $crate::emit_verdict_with(
            $tag, $event, module_path!(), line!(),
            &[$($acc,)* (stringify!($key), $crate::Value::Text(format!("{:?}", $value)))],
            format_args!($($human)+),
        )
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = ? $value:expr) => {
        $crate::emit_verdict(
            $tag, $event, module_path!(), line!(),
            &[$($acc,)* (stringify!($key), $crate::Value::Text(format!("{:?}", $value)))],
        )
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = $value:expr, $($rest:tt)*) => {
        $crate::__hl_verdict_go!($tag, $event, [$($acc,)* (stringify!($key), $crate::Value::from($value))] $($rest)*)
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = $value:expr ; $($human:tt)+) => {
        $crate::emit_verdict_with(
            $tag, $event, module_path!(), line!(),
            &[$($acc,)* (stringify!($key), $crate::Value::from($value))],
            format_args!($($human)+),
        )
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] $key:ident = $value:expr) => {
        $crate::emit_verdict(
            $tag, $event, module_path!(), line!(),
            &[$($acc,)* (stringify!($key), $crate::Value::from($value))],
        )
    };

    ($tag:ident, $event:ident, [$($acc:expr),*]) => {
        $crate::emit_verdict($tag, $event, module_path!(), line!(), &[$($acc),*])
    };
    ($tag:ident, $event:ident, [$($acc:expr),*] ; $($human:tt)+) => {
        $crate::emit_verdict_with(
            $tag, $event, module_path!(), line!(), &[$($acc),*], format_args!($($human)+),
        )
    };
}
