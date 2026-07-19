# hl-log

Foundational **tag-based logging + counters + timing** for the whole app.
std-only, **zero external dependencies**, deliberately minimal so we keep full
control over the hot path. It replaces ad-hoc `eprintln!`/`tracing` scattering with
one consistent, subsystem-tagged, provably-zero-cost-when-off facility.

```rust
use hl_log::tag;

hl_log::hl_info!(tag::GPU, "submit frame {} ({} bytes)", id, n);
hl_log::hl_count!(tag::GPU, "frames");
let _s = hl_log::hl_span!(tag::WGPU, "readback"); // times the enclosing scope
```

---

## Two independent "off = free" axes

hl-log is free-when-disabled at **two** levels, and you can lean on either:

### 1. Compile-time — the build profile decides what exists

| macro | debug | `--release` (default) | `--release --features release-verbose` | `--features disabled` |
|---|---|---|---|---|
| `hl_error!` | compiled | **compiled** | compiled | removed → `{}` |
| `hl_log!` (runtime level) | compiled | **compiled** | compiled | removed → `{}` |
| `hl_warn!` `hl_info!` `hl_debug!` `hl_trace!` | compiled | **removed → `{}`** | compiled | removed → `{}` |
| `hl_count!` `hl_add!` `hl_span!` | compiled | **removed → `{}`** | compiled | removed → `{}` |

In a normal `--release` build the verbose levels and all profiling **expand to `{}`**:
the branch, the `format_args!`, and the argument expressions are physically removed by
the compiler. Only `hl_error!` survives — the one level you always want to be able to
surface in production — plus `hl_log!` as a runtime-level escape hatch.

- `--features release-verbose` — keep everything compiled in release too (for on-demand
  profiling / verbose tracing of a release binary), still runtime-gated.
- `--features disabled` — hard-off **everything**, including `hl_error!`. The branches
  are gone entirely.

Two consts let code introspect the current build:
`hl_log::LOG_COMPILED` (any logging at all) and `hl_log::VERBOSE_COMPILED`.

### 2. Runtime — the gate

Even when a macro is compiled in, it is fronted by:

```rust
#[inline(always)]
pub fn enabled(&self, tags: Tags, level: Level) -> bool {
    self.enabled.load(Relaxed) & tags.bits() != 0
        && (level as u8) <= self.level.load(Relaxed)
}
```

One relaxed atomic load, an AND, a compare, and a branch predicted **not-taken** when
logging is off. With the default configuration the enabled mask is `0`, so a live call site costs
a couple of nanoseconds and **never evaluates its arguments** — `format_args!` lives
*inside* the `if`, so no formatting, no allocation, no side effects happen when off.

**Measured disabled-path cost** (5,000,000 gated calls, `hl_debug!` with a tag that is
off): **~0.23 ns/call** in `--release`, **~8.5 ns/call** in an unoptimized debug build.
Effectively free. (See `tests/logging.rs::disabled_path_is_cheap`.)

---

## Tags

A tag is a single bit in a `u64` mask (up to 64 tags). A call site names one tag (or an
OR of several); the global `ENABLED` mask decides whether it is live.

Predefined: `GPU WGPU VULKAN GL CUDA COMPOSITOR TRANSPORT WIRE PRESENT EXEC SHIM RUNTIME
CPU EGL WAYLAND`. Plus `ALL = !0` and `NONE = 0`.

```rust
use hl_log::tag;
"gpu".parse::<hl_log::Tag>(); // -> Ok(tag::GPU)
tag::VULKAN.name();            // -> "vulkan"
"gpu,wgpu".parse::<hl_log::Tags>(); // -> Ok(tag::GPU | tag::WGPU)
hl_log::hl_debug!(tag::GPU | tag::PRESENT, "..."); // multi-tag: prints "gpu|present"
```

### Adding a new tag

Edit `src/tag.rs`:

1. Add a const with the next free bit: `pub const MYSUB: Tag = Tag::new(1 << 15, "mysub");`
2. Add `MYSUB` to the `TAGS` table.

That's the whole change — `FromStr`, display, application configuration parsing, and every
macro pick it up automatically.

---

## Environment variables

Husklet translates these compatibility variables at its composition root; `hl-log`
itself never reads ambient process state:

| var | meaning | examples |
|---|---|---|
| `HL_LOG` | which tags log (enabled mask) | `gpu,wgpu,transport` · `all` · `off` / empty |
| `HL_LOG_LEVEL` | minimum level (default `warn`) | `error` `warn` `info` `debug` `trace` |
| `HL_LOG_COUNTERS` | which tags collect counters + timing spans | `gpu,transport` · `all` · `off` |

```sh
HL_LOG=gpu,wgpu HL_LOG_LEVEL=debug HL_LOG_COUNTERS=gpu ./app
```

Applications and tests apply one typed configuration:

```rust
hl_log::Config {
    logging: tag::GPU.into(),
    level: hl_log::Level::Debug,
    profiling: tag::GPU.into(),
}.apply();
```

---

## Levels

`Level::{Error=1, Warn=2, Info=3, Debug=4, Trace=5}`. A message passes when
`level <= MIN_LEVEL`, so `MIN_LEVEL = Info` admits Error/Warn/Info and suppresses
Debug/Trace.

---

## Macros

```rust
hl_error!(tag, fmt, args...);   // always compiled unless `disabled`
hl_warn!(tag,  fmt, args...);   // debug-only (or release-verbose)
hl_info!(tag,  fmt, args...);
hl_debug!(tag, fmt, args...);
hl_trace!(tag, fmt, args...);
hl_log!(tag, level, fmt, args...); // runtime Level; always compiled unless `disabled`
```

Emitted line shape:

```
[gpu] D +12ms t3 my_crate::module:214: submit frame 7 (4096 bytes)
 │    │  │     │  └ module:line              └ your message
 │    │  │     └ thread id
 │    │  └ millis since first log line
 │    └ level (E W I D T)
 └ tag name(s), joined by | when several bits are set
```

## Counters

Named `u64` totals, gated by `Config::profiling`. A counter under a disabled tag is a pure
no-op (gate load + branch, no lock), so profiling is opt-in and zero-cost off.

```rust
hl_count!(tag::GPU, "frames");        // += 1
hl_add!(tag::GPU, "bytes", n as u64); // += n

hl_log::Counters::global().snapshot(); // -> Vec<(&str, u64)>, sorted
hl_log::Counters::global().dump();     // pretty table -> sink
hl_log::Counters::global().reset();
```

## Timing spans

`hl_span!` returns a guard that records elapsed time on drop when the tag's counters are
enabled; otherwise the guard is inert and its drop does nothing.

```rust
{
    let _s = hl_log::hl_span!(tag::WGPU, "readback");
    do_readback(); // timed
} // elapsed folded into name -> {count, sum_ns, max_ns}

hl_log::Timings::global().snapshot(); // -> Vec<(&str, TimingStat)>, sorted
hl_log::Timings::global().dump();     // name / count / total ms / avg us / max us -> sink
hl_log::Timings::global().reset();
```

Counters and timing use a **sharded** registry (striped locks by name hash) so concurrent
threads updating different names don't contend, critical sections are O(1) with no I/O
held under lock, and locks are poison-safe (a panic in one thread can't wedge the rest).

## Flushing

```rust
hl_log::flush(); // dump counters table + timing table to the sink
```

`flush()` snapshots under brief per-shard locks, then writes with **no lock held**, so it
never stalls other threads.

## Sinks

Output goes to a swappable `Sink`. Default is `StderrSink` (single locked-stderr write).

```rust
hl_log::set_sink(Box::new(MySink));  // route logs (file, terminal, test collector, ...)
hl_log::reset_sink();                // back to stderr
```

---

## Features

- *(default)* — runtime gate in; verbose + profiling compiled only in debug builds.
- `release-verbose` — keep verbose macros + counters + spans compiled in release too.
- `disabled` — compile-time hard-off; every macro expands to `{}` (even `hl_error!`).

## Build / test

```sh
cargo test  --offline --manifest-path hl-log/Cargo.toml
cargo build --offline --manifest-path hl-log/Cargo.toml --features disabled
```

`hl-log` is its own isolated workspace (empty `[workspace]` in `Cargo.toml`), so it stays
out of any parent workspace.
