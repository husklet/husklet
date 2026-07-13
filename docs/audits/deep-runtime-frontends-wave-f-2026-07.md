# Runtime/frontends deep audit — wave F (2026-07)

This follow-up narrows wave C to symbol-level GUI evidence, exact terminal/screenshot environment
ownership, direct dependency usage, declared binary consumers, and scenario-parser branches. It is a
documentation-only audit; no proposed deletion below has been applied.

## 1. Blanket-suppressed GUI symbols

The suppressed `dd-gui/src/ui` tree contains 49 named functions/types/constants. References were counted
across all Rust sources, then checked for GTK/Relm template, callback, action, CSS-property, and string
lookup uses. This UI is constructed imperatively: there are no `.ui` templates or reflection-based
callback names that can consume free Rust functions by string.

### Proven definition-only candidates

Two functions have exactly one repository occurrence—the definition—and are not callbacks, exported
crate API, template symbols, or property handlers:

- `dd-gui/src/ui/components/widgets/cards.rs:28` — `stat_card`;
- `dd-gui/src/ui/components/widgets/detail.rs:61` — `section_caption`.

Both are `pub(crate)` only because `widgets/mod.rs` glob-reexports every helper. Their replacements are
already live: callers use `sparkline_card` and `section`. They are behavior-neutral removal candidates
after the required macOS/Nix `cargo check -p dd-gui --all-targets` gate.

### Confirmed live suppressed symbols

Every other named helper has at least one call/reference beyond its definition. Representative exact
counts (definition included) are: `network_list_row` 2, `network_detail` 2, `render_settings` 2,
`update_card` 2, `image_detail` 2, `render_workspaces` 2, `render_system` 2, `render_home` 2,
`build_onboarding` 2, `container_list_row` 2, `container_info` 2, `volume_list_row` 2, `volume_detail` 2,
`new_terminal_button` 2, `prompt_switch_context` 2, `confirm_reset` 2, `show_cli_install` 2,
`human_size` 14, `section` 18, and `text_btn` 12. These must not be deleted based on blanket suppression.

The main source of `unused_imports` noise is structural, not hidden dead features:

- `components/widgets/mod.rs` imports broad UI/model/client preludes and glob-reexports five modules;
- `components/dialog/mod.rs`, `components/mod.rs`, and `views/mod.rs` glob-reexport all children;
- each view imports all four `dd_client` models even when it renders one resource.

Replace those glob imports/reexports with explicit names, then remove module-level allowances. This will
let the compiler expose further item-level candidates without deleting live callbacks. CSS names are not
Rust-symbol consumers and do not rescue either definition-only function above.

## 2. Exact `DD_TERM_*` / `DD_SHOT*` producer-consumer map

Repository-wide search excluding audit prose found 13 terminal hooks and three application screenshot
hooks.

| Variable | Runtime reader | Non-document producer | Disposition |
|---|---|---|---|
| `DD_SHOT` | `dd-gui/src/main.rs:201`, shell support comment | `dd-gui/mac/shot.sh:17` | keep; owned harness contract |
| `DD_SHOT_VIEW` | `main.rs:149` | `mac/shot.sh:17` | keep; owned harness contract |
| `DD_SHOT_DELAY_MS` | `main.rs:204` | `mac/shot.sh:17` | keep; owned harness contract |
| `DD_TERM_VIEW` | `dd-gui/src/bin/term.rs:348,3602` | none | zero external consumer |
| `DD_TERM_WS` | `term.rs:351` | none | zero external consumer |
| `DD_TERM_SETTINGS_PANE` | `term.rs:799` | none | zero external consumer |
| `DD_TERM_NEWWS_PANE` | `term.rs:1236` | none | zero external consumer |
| `DD_TERM_TABS` | `term.rs:2106` | none | zero external consumer |
| `DD_TERM_SPLIT` | `term.rs:2112` | none | zero external consumer |
| `DD_TERM_DASH` | `term.rs:2120` | none | zero external consumer |
| `DD_TERM_CMD` | `term.rs:2764` | none | zero external consumer |
| `DD_TERM_DEBUG_LOG` | `term.rs:2765` | none | zero external consumer |
| `DD_TERM_TYPE` | `term.rs:2845` | none | zero external consumer |
| `DD_TERM_DASHPANE` | `term.rs:2996` | none | zero external consumer |
| `DD_TERM_SHOT` | `term.rs:3601` | none | zero external consumer |
| `DD_TERM_SHOT_MS` | `term.rs:3605` | none | zero external consumer |

“None” means no script, test, Make target, workflow, package script, or other source writes or documents an
invocation; rebrand inventory entries are not producers. The 13 terminal hooks form one dormant manual
screenshot/debug interface. Remove them as one branch family unless a terminal screenshot harness is
first checked in. Partial removal would leave coupled behavior: `DD_TERM_VIEW` gates self-capture and
selects initial UI, `DD_TERM_WS` refines its terminal selection, and the pane/tab/split hooks mutate that
selected state. `DD_TERM_CMD` and `DD_TERM_DEBUG_LOG` are diagnostic overrides rather than product
configuration; with no producer or regression test they should not remain indefinitely.

`DD_SHOT*` is different: `mac/shot.sh` actively sets all three. Its path is manual rather than Make/CI
wired, but the producer is concrete. Either wire that script to visual validation or retire script and
readers together; the readers alone are not zero-consumer hooks.

## 3. Direct dependencies and declared targets

Every direct dependency in `dd-daemon`, `dd-cli`, `dd-client`, `dd-gui`, and `dd-images` has a concrete
source use. Notable false positives avoided:

- daemon `hyper`, `hyper-util`, and `tower` are referenced directly in `main.rs`, `http.rs`, and exec
  upgrade handling; `futures-util` constructs logs/stats/events/pull streams;
- CLI `libc` owns PTY/signal/wait operations, Tokio owns the in-process GPU/display runtime and doctor,
  and all five path dependencies have direct launcher/workspace/client uses;
- client `bytes` appears in `log_bytes`' return type, while bollard and `futures-util::StreamExt` implement
  the actual facade;
- GUI serde powers update JSON, Tokio powers process/sleep operations, and libc powers PTY/process-group
  behavior; both internal crate dependencies serve both binaries;
- images serde/serde_json/sha2 cover persisted OCI structures, manifests and in-process digests.

No direct dependency is therefore a proven safe cut. Tool-only unused-dependency output would still need
macOS target and feature validation before changing manifests.

All declared binaries also have consumers: `dd-daemon` and `ddcli` are built by Make, smoke and bundle
flows; `dd-app` is the bundled GUI; `dd-term` has an explicit macOS/Nix launch path and is bundled as the
workspace terminal; `dd-tests` is the default correctness runner; `scenarios` is invoked by four Make
targets. `dd-images/examples/pull_image.rs` remains Cargo's intentional manual example, not an orphan
installed binary.

## 4. Scenario parser and stale behavior-neutral text

`dd-tests/src/bin/scenarios.rs` has two correctness bugs requiring tests rather than cleanup:

- `-t` without a value and `-t nonsense` both fall through `and_then(parse_target).map(...).unwrap_or(targets)`
  and silently retain the previous/default target selection;
- `-c` without a value becomes `None` and silently broadens selection to all categories.

Both should exit 2. Extract argument parsing into a pure Rust function and test missing/unknown values,
repeated flags, and aliases before changing runtime dispatch. The existing `--count` zero-selection check
does not protect normal runs, which boot the daemon before discovering an empty displayed set; normal mode
should reject zero selected cells too.

Behavior-neutral stale material that can be removed or corrected independently:

- `dd-term-core/Cargo.toml`'s `winit + wgpu` shell comment contradicts the GTK4/GSK `dd-term` manifest;
- the scenario module examples say `-t arm` but omit that invalid/missing targets currently do not fail;
- `dd-cli/src/cli.rs` describes CUDA as “presence only, not compute”; verify against the now-wired GPU
  provider and update the help text without changing flag persistence;
- broad unused imports in suppressed GUI modules can be narrowed independently of rendering behavior.

## 5. Ordered cuts

1. Remove `stat_card` and `section_caption`, then prove the macOS all-target GUI build.
2. Replace GUI glob preludes/reexports and delete blanket allowances only after compiler-confirmed cleanup.
3. Either add an owned Rust/script terminal screenshot gate or remove the complete 13-variable dormant
   `DD_TERM_*` branch family.
4. Keep the three `DD_SHOT*` hooks while `mac/shot.sh` remains; decide their ownership atomically.
5. Fix scenario parsing with Rust tests; do not treat parser branches as maintenance-only deletion.
6. Remove no direct dependencies or declared binaries from the audited crates on current evidence.
