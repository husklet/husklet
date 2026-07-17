//! Tags: a `u64` bitmask, one bit per subsystem.
//!
//! A tag is a single bit. A call site names one tag (or an OR of several), and the
//! global `ENABLED` mask decides whether that call site is live. There is room for
//! up to 64 tags. `ALL = !0` matches every tag.
//!
//! # Adding a new tag
//! 1. Add a `pub const NAME: u64 = 1 << N;` below using the next free bit `N`.
//! 2. Add a `(NAME, "name")` row to the `TAGS` table.
//! That is the entire change — `from_name`, `name`, env parsing, and every macro pick
//! it up automatically.

/// A subsystem tag: a single bit in the `u64` enable mask (alias for readability at call sites
/// and in signatures — the consts below are plain `u64`).
pub type Tag = u64;

/// Every tag on.
pub const ALL: u64 = !0;
/// No tags.
pub const NONE: u64 = 0;

pub const GPU: u64 = 1 << 0;
pub const WGPU: u64 = 1 << 1;
pub const VULKAN: u64 = 1 << 2;
pub const GL: u64 = 1 << 3;
pub const CUDA: u64 = 1 << 4;
pub const COMPOSITOR: u64 = 1 << 5;
pub const TRANSPORT: u64 = 1 << 6;
pub const WIRE: u64 = 1 << 7;
pub const PRESENT: u64 = 1 << 8;
pub const EXEC: u64 = 1 << 9;
pub const SHIM: u64 = 1 << 10;
pub const RUNTIME: u64 = 1 << 11;
pub const CPU: u64 = 1 << 12;
pub const EGL: u64 = 1 << 13;
pub const WAYLAND: u64 = 1 << 14;

/// The registry of predefined tags: `(bit, lowercase name)`.
///
/// Order matters only for the deterministic output of [`name`] when several bits
/// share a call (the first matching name wins) and for `HL_LOG` name listing.
pub const TAGS: &[(u64, &str)] = &[
    (GPU, "gpu"),
    (WGPU, "wgpu"),
    (VULKAN, "vulkan"),
    (GL, "gl"),
    (CUDA, "cuda"),
    (COMPOSITOR, "compositor"),
    (TRANSPORT, "transport"),
    (WIRE, "wire"),
    (PRESENT, "present"),
    (EXEC, "exec"),
    (SHIM, "shim"),
    (RUNTIME, "runtime"),
    (CPU, "cpu"),
    (EGL, "egl"),
    (WAYLAND, "wayland"),
];

/// Resolve a lowercase tag name to its bit. Also accepts `"all"` and
/// `"none"`/`"off"`. Case-insensitive. Returns `None` for unknown names.
pub fn from_name(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => None,
        "all" => Some(ALL),
        "none" | "off" => Some(NONE),
        _ => TAGS.iter().find(|(_, n)| *n == s).map(|(b, _)| *b),
    }
}

/// The lowercase name for a single tag bit. If several bits are set, the first
/// matching name in [`TAGS`] is returned. `ALL` yields `"all"`, `0` yields `"-"`,
/// and an unnamed bit yields `"?"`.
pub fn name(bit: u64) -> &'static str {
    if bit == ALL {
        return "all";
    }
    if bit == 0 {
        return "-";
    }
    for (b, n) in TAGS {
        if bit & *b != 0 {
            return n;
        }
    }
    "?"
}

/// Write every set tag name into `out`, joined by `|` (e.g. `gpu|wgpu`). Used by
/// the emitter so a multi-tag call shows all its tags.
pub fn write_names(bit: u64, out: &mut String) {
    if bit == ALL {
        out.push_str("all");
        return;
    }
    if bit == 0 {
        out.push('-');
        return;
    }
    let mut first = true;
    for (b, n) in TAGS {
        if bit & *b != 0 {
            if !first {
                out.push('|');
            }
            out.push_str(n);
            first = false;
        }
    }
    if first {
        out.push('?');
    }
}
