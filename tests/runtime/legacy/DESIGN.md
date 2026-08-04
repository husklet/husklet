# Compatibility test boundary

The checked C sources and dual-ISA prebuilts are guest inputs. Rust integration
tests consume the declarative manifest and drive the public engine composition;
the guest language does not dictate the host test language.

Every case runs in a fresh worker process with a fresh engine and workspace.
The worker copies the immutable prebuilt into its workspace, captures the
structured exit and output, destroys the engine, and removes the workspace.
The supervisor owns timeout and crash containment, so a wedged engine cannot
prevent teardown of later cases.

On POSIX the worker is a process-group leader. The worker also converts parent
HUP, INT, and TERM into a cooperative engine stop/destroy before exiting.
Normal completion reaps the leader and waits boundedly for containment to
quiesce; timeout, output overflow, and detected stall send TERM, wait two
seconds, then send KILL and reap. Linux records descendant PID/start-time
identities before teardown, so engine or authority children that create a new
group or session are still killed without risking a reused PID. Linux
stall progress is the union of process-tree CPU ticks and capture-file growth,
sampled once per second. An unavailable CPU source deliberately disables the
stall decision while leaving the row wall deadline active. Captures have
independent 1 MiB stdout and 64 KiB stderr termination thresholds. Windows
still needs the C oracle's suspended-launch Job Object boundary and exact job
CPU accounting;
direct-child termination there is not sufficient for the final matrix gate.

`multi-process-service` describes a capability of the guest runtime, not an
external host helper. The worker launches one persistent C binary through the
public Builder. Any fork, clone, pthread, wait, or exec topology remains inside
that engine instance. This matches the retained C matrix runner and preserves a
single lifecycle, output stream, timeout boundary, and teardown obligation.

The eight bootstrap case/ISA rows are the first blocking API gate. Scaling to
`inventory.tsv` extends manifest/setup parsing only. The retained C baseline is
an optional, separate differential input and is not the Rust gate's host API.

`src/app/hl-engine/tests/inventory.rs` consumes the complete cross-host artifact
union and applies the retained runner's per-host disposition rule before launch.
`active` runs everywhere, `excluded-macos` runs off macOS, and
`excluded-windows` runs off Windows; global exclusions never enter the artifact
union. It writes concise `report/api-results.tsv` only for an unfiltered
complete run.
Suite, ISA, and case substring filters are `HL_COMPAT_SUITE`, `HL_COMPAT_ISA`,
and `HL_COMPAT_CASE`; filtered runs use deterministic filter-specific report
names so they cannot replace the canonical combined result. `HL_COMPAT_REPORT`
selects an explicit output path. Bounded parallelism uses `HL_COMPAT_JOBS` (1
through 32) and defaults to one. Artifact, expected exit, stdout/stderr
goldens, timeout, environment options, and dependency declarations are carried
per row. Dependency tags are retained in results. Rootfs support is category
driven: scratch, mapping-data, Alpine directory skeleton, and dynamic loader
roots map to bounded typed `Rootfs`/`Input` projections. Unknown rootfs labels
remain unsupported, so adding a dependency tag cannot silently select host
filesystem behavior. Staging rejects traversal and source-symlink following,
preserves file modes, applies one aggregate copy limit, and rolls the owned
workspace back on setup or launch failure.

Imported deadlines preserve the retained C runner contract: ordinary cases carry
120,000 ms, while `soak` carries the explicit 240,000 ms CMake override. The
default `compatibility` profile uses those values unchanged. An explicit
`HL_COMPAT_TIMEOUT_PROFILE=performance` profile caps each deadline at 10,000 ms
for fast performance regression evidence; it is intentionally stricter and is
not compatibility verdict evidence. The selected profile is resume-fingerprinted.

`HL_COMPAT_RESUME=1` enables a fingerprinted partial ledger adjacent to the
selected report. `HL_COMPAT_BATCH=N` then executes at most N pending rows per
invocation. The fingerprint binds the inventory, fixture schema, selected guest
and golden bytes, side inputs, runtime-root resources, actual runner/worker/ISA
and authority binaries, host OS/architecture, filters, and outcome-changing
settings. A host-released exclusive lock prevents concurrent owners. Every row
is synchronized before progress is claimed; restart drops only one torn final
non-newline suffix and rejects interior corruption, stale, duplicate, or foreign
rows. The canonical report replaces the partial ledger only after every
selected row has a durable result.
