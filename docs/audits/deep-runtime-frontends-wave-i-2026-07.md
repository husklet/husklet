# Runtime/frontends deep audit — wave I (2026-07)

This docs-only pass expands wave F's deletion candidates through branch/state tracing and Git history.

## Dormant `DD_TERM_*` family: exact boundary

All 13 names first appear with the entire 3,905-line `dd-gui/src/bin/term.rs` in commit `3cc0601f`
(2026-07-08). `git log -S` finds no earlier producer, and repository history outside that file contains no
script, Make target, workflow, or test producer. Commit `f4e58abc` only added documentation mentions.
Therefore these were checked in as manual hooks, not remnants of a once-wired harness.

The family occupies approximately 125 source lines across nine branches plus one helper. Exact state and
unset behavior:

- `DD_TERM_VIEW`/`DD_TERM_WS` (`term.rs:348-369`) can open a second terminal/new-workspace/settings
  window after the manager is presented. Unset takes the wildcard arm and performs no store load/window
  open. `DD_TERM_VIEW` is also read by capture selection.
- `DD_TERM_SETTINGS_PANE` (`:798-810`) and `DD_TERM_NEWWS_PANE` (`:1235-1247`) mutate visible stack page
  and nav CSS only when present. Unset leaves the normal page selected by construction.
- `DD_TERM_TABS`, `DD_TERM_SPLIT`, and `DD_TERM_DASH` (`:2105-2127`) layer mutations after normal session
  restore/fresh-tab creation. Unset performs three environment lookups but no model/widget mutation.
- `DD_TERM_SPLIT` exclusively motivates recursive `first_terminal_in` (`:2605-2615`): its only external
  call is the split hook; the recursive occurrence is internal. Delete helper with that branch.
- `DD_TERM_CMD`/`DD_TERM_DEBUG_LOG` (`:2762-2786`) replace normal `ddcli workspace launch` argv. Unset
  allocates two `None` options and takes `launch_args`; removing the branch restores that exact path.
- `DD_TERM_TYPE` (`:2845-2850`) schedules synthetic input only after successful PTY spawn. Unset adds no
  callback and does not affect the child.
- `DD_TERM_DASHPANE` (`:2995-3007`) changes stack page/sidebar CSS only when set.
- `DD_TERM_SHOT`, `DD_TERM_SHOT_MS`, and the capture-side `DD_TERM_VIEW` live in `maybe_shot`
  (`:3597-3625`). The function is called at four window presentation sites (`:345`, `:813`, `:1250`,
  `:2130`) but returns immediately on absent `DD_TERM_SHOT`; unset cost is four environment lookups only.

No branch seeds state later consumed by normal startup when unset. The safe deletion boundary is therefore:
all nine conditional blocks, four `maybe_shot` calls, `maybe_shot` itself, and `first_terminal_in`. Keep
the normal manager/window builders, session restoration, `launch_args`, PTY spawn, dashboard creation, and
page construction. Removing only environment reads while retaining forced mutations would be unsafe;
removing the complete boundary is behavior-neutral for every repository-owned invocation.

## Scenario parser: exact refactor boundary

Argument selection is confined to `parse_target` and the `argv` loop in
`dd-tests/src/bin/scenarios.rs:53-111`; runtime configuration begins with repository-path construction at
`:112`. Extract those parser lines into `fn parse_args(...) -> Result<Selection, String>` without moving
daemon boot, scheduling, or result accounting.

Required semantics to lock with Rust tests:

- default: backend dd, quick class, both Linux targets, no category/count;
- accepted target aliases currently encoded by `parse_target`;
- missing values for `--backend`, `-c/--category`, and `-t/--target` exit 2;
- unknown backend/target and unknown flag exit 2;
- repeated scalar flags have an explicit last-wins policy (matching current behavior);
- both count and normal execution reject zero selected scenario×target cells before daemon boot.

The present `-t` implementation preserves the previous target on missing/invalid values; `-c` missing
silently becomes no filter. Fixing these is behavior-changing correctness work, not a no-risk deletion.
The behavior-neutral cuts are stale inline parsing comments after extraction and duplicated usage text once
clap/one canonical help string owns it.

## GUI definition-only helpers: exact deletion boundary

`stat_card` is exactly `dd-gui/src/ui/components/widgets/cards.rs:28-46`; `section_caption` is exactly
`components/widgets/detail.rs:61-69`. Neither has a call, template/callback/property string, test, or
public export outside the crate. Both become visible only through `pub(crate) use cards::*` / `detail::*`
in `widgets/mod.rs`; those glob exports need not change to delete the functions.

Do not delete adjacent live helpers: `setting_card` has four occurrences, `sparkline_card` five,
`section` eighteen, and `detail_header` five. The minimal proof sequence is remove only the two function
bodies, run formatting, then macOS/Nix `cargo check -p dd-gui --all-targets`. CSS classes used only by
`stat_card` are not yet safe cuts: `dd-stat-card`, `dd-stat-value`, and `dd-stat-name` are also consumed by
`sparkline_card`.

## `dd-tests` blanket `unused_imports`

Twenty-four `dd-tests/src/cases/ext/*.rs` modules suppress unused imports. These files do not use generated
macros that require names to be pre-imported: `group`, `src`, `port`, `fixture`, and `in_rootfs` are ordinary
builder functions, and chained methods resolve through inherent `Case` methods rather than importing the
`Case` type. Word-occurrence analysis (an identifier appearing only in its `use crate::{...}` line) gives
the exact removable imports:

- `threads`: `fixture`, `in_rootfs`, `Case`, `Engine`;
- `soak`: `darwin_libc`, `darwin_src`, `fixture`, `in_rootfs`, `Case`;
- `clitools`: `fixture`, `port`, `src`, `Engine`; `dentry`: `Case`;
- `timex`: `fixture`, `in_rootfs`, `Case`; `abi`: `fixture`, `in_rootfs`, `Engine`;
- `linuxsys`: `fixture`, `in_rootfs`, `Case`; `fs`: `fixture`, `in_rootfs`, `port`, `Case`;
- `processx`: `fixture`, `in_rootfs`, `Case`; `isolation`: `Case`; `memory`: `src`;
- `procexe`: `fixture`, `in_rootfs`, `port`, `Case`;
- `darwin`: `fixture`, `in_rootfs`, `src`, `Case`, `Engine`;
- `memx`: `fixture`, `in_rootfs`, `Case`; `fsx`: `fixture`, `in_rootfs`, `Case`, `Engine`;
- `signalx`: `fixture`, `in_rootfs`, `Case`; `net`: `fixture`, `in_rootfs`, `Case`, `Engine`;
- `posix`: `fixture`, `in_rootfs`, `Case`; `libc`: `fixture`, `in_rootfs`, `Case`, `Engine`;
- `ipc`: `fixture`, `in_rootfs`, `Case`; `elf210`: `Case`; `procfs`: `Case`;
- `pcachex`: `darwin_libc`, `darwin_src`, `fixture`, `in_rootfs`, `port`, `Engine`.

`syscallcompat.rs` has no definition-only imported identifier, so its blanket allowance itself is a
no-risk candidate. For every module, remove the listed imports and the allowance together, then let
`cargo check -p dd-tests --all-targets` expose any missed cfg-sensitive use. There are no `#[cfg]`-guarded
imports in these listed lines, so this cleanup does not change case registration or generated guests.

## Additional no-risk text/symbol cuts

- Delete obsolete “Owner: … agent. Edit ONLY this file” comments throughout these case modules; transient
  agent allocation is not source ownership and becomes false immediately after integration.
- Replace repeated “Keep this module compiling at all times” comments with the workspace test policy once;
  Cargo already compiles registered modules.
- Correct `dd-term-core/Cargo.toml`'s obsolete `winit + wgpu` statement to GTK4/GSK.
- Keep `DD_SHOT*` application hooks: unlike `DD_TERM_*`, all three have a real producer in
  `dd-gui/mac/shot.sh`.

This pass authorizes no code changes; it records minimal boundaries for a subsequent reviewed cleanup.
