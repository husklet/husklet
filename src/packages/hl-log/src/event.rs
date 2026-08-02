//! Structured events: one machine-readable record per line, beside the human sentence.
//!
//! # Why
//!
//! Every diagnostic in this tree formats a sentence, so the structure is destroyed at the call site and
//! every consumer becomes a `grep`. That is not a stylistic complaint. A grep returned the first line of
//! a multi-line error while the answer sat four lines below it; twelve "crash signatures" turned out to
//! be log lines containing the substring `segmentation`; a `head -25` could not have reported an absence
//! however it was read. A record with stable keys is selectable without pattern-matching prose, and an
//! absent key is visibly absent rather than silently unmatched.
//!
//! # What a record looks like
//!
//! One JSON object per line, on the events sink, with provenance first and the caller's fields after:
//!
//! ```text
//! {"event":"command_buffer.refused","tag":"vulkan","level":"error","at":"hl_vulkan::model::device:190",
//!  "ms":1234,"thread":1,"buffer":21,"reason":"copy between incompatible formats"}
//! ```
//!
//! `event`, `tag`, `level`, `at`, `ms` and `thread` are always present and always mean the same thing.
//! `at` is the module and line the record came from, which is the field that answers "which of the two
//! call sites produced this count" — a question that voided three conclusions in one session, when a
//! counter reporting `parked=3 refused=3` turned out to sit on a different path from the line that would
//! have named the refusal.
//!
//! # Typed fields
//!
//! A field is typed, so a consumer gets a number where the caller wrote a number:
//!
//! ```
//! # use hl_log::{hl_event, tag, Level};
//! hl_event!(tag::GPU, Level::Debug, "frame.lowered", draws = 12, scissored = true, target = "swapchain");
//! ```
//!
//! Integers, floats and bools land as JSON numbers and booleans; strings as strings. For a type that is
//! neither, mark it with the sigil that says how to render it — `%` for `Display`, `?` for `Debug`:
//!
//! ```
//! # use hl_log::{hl_event, tag, Level};
//! # let error = "oh no"; let id = 7u32;
//! hl_event!(tag::GPU, Level::Error, "submit.refused", id = id, reason = %error);
//! ```
//!
//! # What is NOT changed
//!
//! The tag mask and the compile-time level policy are exactly as they were. `hl_event!` at a verbose
//! level compiles to nothing in release, just like `hl_warn!`; a closed tag still costs one relaxed load
//! and a predicted branch, and never evaluates its arguments. The one deliberate exception is
//! [`crate::hl_verdict!`], which is documented where it is defined.

use crate::level::Level;
use crate::sink;
use crate::tag::Tags;
use std::fmt::Write as _;

/// One field value, typed so a consumer reads a number as a number.
///
/// Deliberately small. A logging package that grows a serialization framework has become a dependency
/// rather than a foundation, and the fields these events carry are identifiers, counts, sizes, flags and
/// reasons. Anything richer renders through `%` or `?` at the call site, where the author can see what it
/// will cost.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl Value {
    fn write(&self, out: &mut String) {
        match self {
            Value::Int(v) => {
                let _ = write!(out, "{v}");
            }
            Value::Uint(v) => {
                let _ = write!(out, "{v}");
            }
            // JSON has no NaN or Infinity. Emitting a bare `NaN` produces a record no parser accepts, so
            // a non-finite float becomes a string naming itself: the consumer still sees the value, and
            // the line stays machine-readable, which is the whole point of the channel.
            Value::Float(v) if !v.is_finite() => {
                let _ = write!(out, "\"{v}\"");
            }
            Value::Float(v) => {
                let _ = write!(out, "{v}");
            }
            Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Value::Text(v) => write_json_string(out, v),
        }
    }
}

macro_rules! from_int {
    ($($t:ty),+) => { $(impl From<$t> for Value {
        fn from(v: $t) -> Self { Value::Int(v as i64) }
    })+ };
}
macro_rules! from_uint {
    ($($t:ty),+) => { $(impl From<$t> for Value {
        fn from(v: $t) -> Self { Value::Uint(v as u64) }
    })+ };
}
from_int!(i8, i16, i32, i64, isize);
from_uint!(u8, u16, u32, u64, usize);

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v as f64)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::Text(v.clone())
    }
}

/// Escape into a JSON string literal, quotes included.
///
/// A reason field carries a driver error, and driver errors contain quotes, backslashes and newlines.
/// One unescaped newline turns a record into two unparseable halves — a machine-readable channel that is
/// only readable when the payload happens to be tame is the same trap as a grep.
fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Format and dispatch one structured record. Reached only when the caller's gate passed.
///
/// Not `inline`: the cold formatting path stays out of the call site, exactly as [`crate::emit`] does.
pub fn emit_event(
    tags: Tags,
    level: Level,
    event: &str,
    module: &str,
    line: u32,
    fields: &[(&str, Value)],
) {
    let mut out = String::with_capacity(128);
    out.push_str("{\"event\":");
    write_json_string(&mut out, event);
    out.push_str(",\"tag\":");
    write_json_string(&mut out, &tags.to_string());
    out.push_str(",\"level\":");
    write_json_string(&mut out, level.name());
    out.push_str(",\"at\":");
    let mut at = String::with_capacity(module.len() + 8);
    let _ = write!(at, "{module}:{line}");
    write_json_string(&mut out, &at);
    let _ = write!(
        out,
        ",\"ms\":{},\"thread\":{}",
        crate::emit::millis_since_start(),
        crate::emit::thread_id()
    );
    for (key, value) in fields {
        out.push(',');
        write_json_string(&mut out, key);
        out.push(':');
        value.write(&mut out);
    }
    out.push_str("}\n");
    sink::Events::global().write(&out);
}

/// A verdict: the same record, plus a human sentence on the normal log, and no tag gate.
///
/// The record and the sentence carry the same fields, so a person reading stderr and a tool reading the
/// events channel are looking at one fact rather than two descriptions of it.
/// A verdict whose human half is the caller's own sentence rather than a rendering of its fields.
///
/// The record and the sentence are the same fact for two readers, and the sentence is where the
/// reasoning lives — a good diagnostic says what the refusal costs and what to look at next, which no
/// set of key-value pairs reproduces.
pub fn emit_verdict_with(
    tags: Tags,
    event: &str,
    module: &str,
    line: u32,
    fields: &[(&str, Value)],
    human: std::fmt::Arguments,
) {
    emit_event(tags, Level::Error, event, module, line, fields);
    crate::emit::emit(tags, Level::Error, module, line, human);
}

pub fn emit_verdict(tags: Tags, event: &str, module: &str, line: u32, fields: &[(&str, Value)]) {
    emit_event(tags, Level::Error, event, module, line, fields);
    // The human half. Rendered here rather than at the call site so the two cannot drift, and so a
    // verdict costs the caller exactly one macro.
    let mut sentence = String::with_capacity(96);
    sentence.push_str(event);
    for (key, value) in fields {
        sentence.push(' ');
        sentence.push_str(key);
        sentence.push('=');
        match value {
            Value::Text(text) => sentence.push_str(text),
            other => other.write(&mut sentence),
        }
    }
    crate::emit::emit(
        tags,
        Level::Error,
        module,
        line,
        format_args!("{sentence}"),
    );
}

#[cfg(test)]
#[path = "event/tests.rs"]
mod tests;
