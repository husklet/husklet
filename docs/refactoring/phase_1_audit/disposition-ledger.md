# Phase 1 current-tree disposition ledger

Verified against the 2026-07-13 tree after the rendering merges. This ledger is the actionable layer over
the historical research. Re-check the named evidence at implementation time; a row is not deletion
authorization.

| ID | Candidate | Current evidence | Disposition | Required proof |
|---|---|---|---|---|
| C01 | `scratch-erl/**` captured rootfs/artifacts | 40,657 baseline paths, no maintained consumer | remove after extracting unique checkpoint probes | Rust/C checkpoint behavior, reproduction recipe, no tracked caller |
| C02 | benchmark binary/gates/16 C fixtures | isolated from correctness; user approved removal | delete atomically with Cargo/Make/docs registrations | correctness suites unchanged; no benchmark invariant left in CI |
| C03 | `txln_has`, `add_pend`, `synth_stat`, `ipc_ns_key` | definitions still present; unity compiler proved private/unused | safe source deletion group | all three unity TUs; proc/sys behavior; cross-container IPC test |
| C04 | x86 local `mem` in translate case | compiler-proven unused | remove with C03 | x86 syntax/build and translated case test |
| C05 | `rare.c` canonical syscall 201 handler | still shadowed by earlier x86 normalizer return | remove unreachable handler only | x86 `time(2)` guest behavior |
| C06 | Vulkan `StaticPtr<T>` | still definition-only in `wl_present.rs` | safe private cut | shim all-target builds for both guest architectures |
| C07 | wgpu `legacy_msl` and helper-only test | still no production/example/cfg caller | safe internal API cut after mac check | wgpu mac all-target; preserve malformed/legacy shader behavior tests |
| C08 | `dd-images::tar_members_contained` duplicate | still unused; daemon owns a tested duplicate | delete or make image extraction use it; do not leave dead security claim | archive traversal tests at actual extraction boundary |
| C09 | GUI `stat_card`, `section_caption` | still definition-only | safe source cut | mac GUI all-target build and view smoke |
| C10 | empty `dd-gpu` `metal`/`cuda` features | declarations still have no cfg/dependency consumers | remove feature names and misleading executor comments | cargo metadata, all features, external invocation search |
| C11 | generated GL string aliases | generator still emits unused aliases | remove emissions, keep byte/list/count forms | generated output + GL query tests |
| C12 | software `ShaderModule::Spirv(Vec<u32>)` payload | bytes still copied and never read | change to opaque unit state; not shader acceptance policy | create/destroy/replay rollback and unsupported-execution behavior |
| C13 | ARM-B1 `IBPROF/VDBETRACE/VTHITCOUNT/CTXDISP` | all still present; no maintained producer; large disabled BSS | delete as one experiment group | linked-size map, ARM engine matrix, IBTC/stitch/pcache behavior |
| C14 | dense fd pathname/state tables | compatibility limit is live; allocation shape is wasteful | sparse/page allocation refactor, not feature deletion | fd-limit/stress parity and syscall hot-path measurement |
| C15 | default-off checkpoint/forkserver/sentry storage | features are live | lazy allocation only | each feature on/off and memory evidence |
| C16 | JIT A/B switches | several are emergency/correctness controls; cache key incomplete | classify individually; fix cache identity before retirement | cold/warm differential runs and explicit owner |
| C17 | 13 dormant `DD_TERM_*` hooks | readers still present; no historical producer for non-shot family | remove family only after phase-2 GUI behavioral replacement | GUI launch/state vectors; preserve `DD_SHOT*` producer |
| C18 | source-substring correctness tests | exact tests still exist in GPU capability test | replace/remove in phase 2 destination | API/wire/pixel behavioral evidence |
| C19 | unused imports/test `_touch` helper/transient agent comments | exact low-risk text/test debris | safe hygiene batch | Rust compile, no source-presence tests |
| C20 | hard-coded `/Users/x/dd/poc/images` fallbacks | still present in scenario paths | central resolver during phase 2 | explicit env/home/temp precedence tests |
| C21 | checked-in guest executable/source pairs | speed/toolchain utility remains | retain; add reproducibility manifest | source hash, compiler identity, rebuild/hash comparison |
| C22 | manual legacy compositor | still the fallback/default when Smithay not selected | retain until Smithay is unconditional | live Chrome/GTK/Vulkan, protocol/pixel/input/HiDPI gates |
| C23 | exact compositor constants/transforms/blend/input normalization | duplicated across legacy/Smithay | centralize exact contracts only | cross-path vectors, optimized hot-loop inspection |
| C24 | `surface_augmenter` | private legacy global remains default-off | remove after one current Chrome registry/bind trace | Chrome startup/navigation trace with no bind |
| C25 | state-only Wayland globals | advertised but incomplete | implementation tasks, not cleanup | protocol clients and application behavior |
| C26 | daemon empty `/plugins` and `/images/search` | Docker compatibility behavior | retain | Docker-client compatibility tests |
| C27 | daemon false-success/fabricated metadata routes | observable incorrect behavior | truthful implementation/error, not deletion | route-level Docker API behavior |
| C28 | workspace tab-format reader | recent migration reader; canonical writer differs | migrate-on-load then timed retirement | oldest fixture, rewrite, next-release policy |
| C29 | image/store/archive/alias representations | distinct lifecycle/ownership | retain fields; centralize typed store metadata | round trips, unknown-key preservation, startup timing |
| C30 | cross-format RGBA pipeline fallback | invalid semantic fallback | correctness fix, not cleanup | all advertised attachment formats on Metal |
| C31 | IOSurface +1 reference and per-spawn env-array leaks | ownership bugs confirmed | correctness/RAII fixes | repeated-create/spawn resource tests |
| C32 | redundant `available()` path check | duplicate of `jit_path` | safe simplification | path missing/race/spawn authority tests |
| C33 | Relm4 default CSS feature | no project use found | validate feature narrowing | feature tree, every GUI view, packaged app |
| C34 | Bollard/Naga/objc feature narrowing | target/generated uncertainty | experimental validation only; no bulk cut | target-specific feature trees and product gates |
| C35 | old macOS image builder | still called by dev/scenario paths | migrate callers before removal | equivalent image contents and launch behavior |

## Labels used during implementation

- **CUT:** behavior-neutral removal with direct reachability/compiler evidence.
- **REFACTOR:** same public behavior with less allocation/duplication; performance proof required on hot paths.
- **FIX:** existing behavior is false, unsafe or leaky; deleting the symptom is not acceptable.
- **MIGRATE:** compatibility/persisted/architectural dependency must move first.
- **KEEP:** intentional compatibility, fallback, fixture, ABI or performance surface.

Each cleanup PR must name ledger IDs and change their state. Completed rows leave the active ledger and are
recorded in Git history; the ledger must not grow into another permanent landed-work diary.
