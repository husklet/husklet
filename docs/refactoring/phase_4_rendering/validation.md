# Phase 4 validation snapshot

Snapshot date: 2026-07-13. Validation used current Rust/C implementation, behavioral tests and reachable Git
history. It did not treat source-string sentinels as correctness evidence.

## Removed from the backlog

| Former claim | Current evidence | Result |
|---|---|---|
| Vulkan must be reduced to a lower truthful profile | `dd-shim-vk` advertises core 1.4; generated tests require real bodies for all 234 mandatory commands through 1.4; `1a49beff` is reachable from main | complete; old 1.0 narrative removed |
| GLES2/EGL mandatory bodies and context/error model | public API tests cover mandatory bodies, distinct/shareable contexts, per-thread current state, release-thread and error lifecycle | complete |
| instanced/base-vertex transport is missing | shared IR carries instance count, first instance and base vertex, with encoder/decoder and executor tests; `60ffa1ec` is reachable from main | complete |
| CUDA accepts unsupported PTX | parser/execution tests reject unsupported PTX before execution | complete |
| dmabuf-v4 device id/feedback basics are missing | explicit Linux `u64` device identity, feedback/generation and resource tests exist in compositor coverage | complete as a primitive; live application parity remains R6 |
| malformed streams partially mutate state | decode/capability rejection tests cover atomic failure before backend mutation | complete for modeled streams |

## Retained after validation

| Objective | Why it remains |
|---|---|
| asynchronous completion (R1) | current acknowledgement does not independently prove accepted versus GPU-completed work across executors |
| GLES query results (R2) | lifecycle is implemented, but backend counters resolve to a deliberate zero |
| Cocoa visible timing (R3) | presenter reads native timing, but no required visible multi-frame device journey closes it |
| XWayland supervision (R4) | model tests exist; default runtime activation does not start and supervise the feature-gated server/XWM path |
| Chrome content (R5) | browser chrome can render, while default multi-process page content still depends on the retained engine fix plan |
| Smithay app parity (R6) | protocol/unit coverage does not yet prove the unmodified workload matrix or permit legacy-compositor deletion |
| production shader/query breadth (R7) | generated inventories cannot establish semantics exercised only by real application traces |

Re-run this reconciliation whenever one of R1–R7 lands. Remove the completed row from `backlog.md`; do not add
a new historical diary here.
