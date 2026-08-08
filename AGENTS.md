# Husklet instructions

These rules define the durable architecture, safety, coding, testing, and delivery
standards for Husklet and its integrated Rust engine. Apply them to every new package and improve
nearby code when doing so preserves behavior and remains within scope.

The retired C engine in `../engine` is a read-only behavioral and performance oracle
during migration. Husklet is the active repository and owns the Rust engine, containers,
workspaces, terminal, and desktop application. Do not add GPU, graphics translation,
surface, compositor, CUDA, OpenGL, Vulkan, or Wayland implementation back into this
repository. Never edit `../engine` while studying it.

## Reading code: CodeGraph first

This repository is indexed by CodeGraph (`.codegraph/` at the root). Reach for it
**before** grep, find, or opening files, both to answer a question and before
editing a symbol. One `codegraph_explore` call returns the verbatim,
line-numbered source of the matching symbols grouped by file — safe to edit from,
and equivalent to having read them — plus the call path among them and a blast
radius naming every caller and the tests that cover each symbol. Prefer the MCP
tool `codegraph_explore`; `codegraph explore "<names>"` in a shell prints the same
output when the tool is unavailable.

The blast radius reports **`no covering tests found`** per symbol. Treat that as a
first-class signal: it names the places where a green suite proves nothing, which
is where this codebase has repeatedly hidden defects.

Two failure modes, both observed:

- **Query precise symbol names, two to four at a time. Never bare filenames.**
  A filename matches repo-wide — `pool.rs` pulls in unrelated container and
  launcher files and spends the whole budget on them.
- **Output is budget-truncated and truncation is silent.** A broad query can drop
  the symbol you asked about and leave it visible only in the blast radius. If the
  source you needed is not in the reply, ask again with fewer, narrower names
  rather than assuming it does not exist.

Do not re-open a file whose source CodeGraph already returned.

`no covering tests found` is a lead, not a verdict. It has misfired repeatedly,
including on symbols whose removal reddens both unit and integration tests.
Confirm a gap by mutating the symbol and watching the suite, which is the standard
this repository requires for a coverage claim anyway.

## Reading a profile

High self-time and removable cost are independent properties, and this engine has
produced both failure modes:

- **Misattributed self-time.** `with_execution_memory` compiles to 4032 bytes
  because the guest-slice closure is inlined wholesale into it, so its row credits
  work done by its callees. Disassemble before believing a row; a function whose
  body should be twenty instructions and measures a thousand is reporting someone
  else's cost.
- **Real self-time that is still free to keep.** `ReservationEpochs::invalidate_at`
  is a genuine 112-byte function and its row is honestly its own, but deleting it
  along with all 5.17 billion of its atomics changed nothing measurable, because
  the `ldadd` discards its result and retires without blocking anything.

So a profile row justifies investigating a symbol. Only a mutation justifies
believing the cost can be recovered.

## Time-to-evidence and agent utilization

Elapsed time to authoritative compatibility evidence is the primary operational
optimization target. CPU is not a scarce resource for repository work: use every
logical CPU when a test, corpus run, compilation, or independent analysis can
benefit from it. Do not serialize work merely to keep CPU utilization low, and do
not default compatibility runs to one worker when the host can safely run more.

RAM, disk space, process-table health, and source/build ownership remain hard
constraints. Before and during wide execution, monitor available memory, swap,
free disk, output growth, and zombie or escaped descendants. Bound per-worker
captures and timeouts, preserve resumable results, and reduce concurrency only
when measured RAM, disk, thermal, or lifecycle evidence requires it. A slow run
must report whether it is limited by CPU, memory, disk, fixture setup, process
startup, locking, or guest timeouts; unexplained serialization is not acceptable.

Keep all available Codex subscriptions and agent slots productively occupied.
Managers must continuously delegate broad, independent, non-overlapping migration
domains, require direct C-oracle and Rust-source study, and replace a completed
assignment with the next highest-value compatibility gap immediately. Each Codex
manager should use its own subagent capacity fully. Coordinate shared-tree edits
and build ownership so maximum agent utilization does not create conflicting
patches or invalidate evidence. Prefer parallel read-only audits while one owner
performs a shared-tree build or authoritative run.

Keep implementation sessions short-lived and outcome-bounded. A normal lane owns
one coherent capability for at most 20 minutes; an external manager coordinating
several independent subagents has a hard 30-minute lifetime. It must then deliver
one audited commit with exact-tree evidence, or a concise source-backed blocker
report, and exit. Repeated diagnosis, fixture-by-fixture iteration, or widening
the lane after its original capability is exhausted is not progress: stop that
session and give a fresh agent the next bounded domain. Preserve unfinished work
on its branch or worktree; never manufacture a cosmetic commit merely to meet the
deadline.

Compatibility workers receive engine launch options only through the typed
`HL_COMPAT_ENGINE_OPTIONS` setting (for example
`HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1'`).
Setting an engine option such as `HL_NATIVE_EXECUTION` directly in the inventory
supervisor's ambient environment does not configure the guest engine and must
never be cited as native-mode evidence. Before a long run, prove the selected
mode with one fast row and require the corresponding native diagnostics.

A commit may be called stable or buildable only after verification from that exact
committed tree. A passing build in a dirty shared worktree is not evidence for
`HEAD`: uncommitted companion schema, match, generated, test, or composition edits
may be supplying the successful build. Before handing a revision to another lane
or starting an authoritative corpus run, verify it in a clean detached worktree
or equivalent clean checkout and record the tested commit. Do not continue shape-
changing edits until the dependent verification has captured a coherent commit.

## Mission

Provide isolated, reproducible Linux workspaces backed by a memory-safe,
high-performance Rust execution engine. Opening a workspace enters its configured
image with a terminal, filesystem, networking, VPN, and container services.

Preserve exact Linux behavior across AArch64 and x86-64 guests and Linux, macOS,
and Windows hosts. The product composes replaceable engine, container, workspace,
and terminal capabilities; reusable crates contain no Husklet product policy.

Ordinary CLI and terminal applications must run without application-specific engine
workarounds. The final compatibility/performance gate includes container workflows,
interactive terminal workloads, and nested engine execution such as `arm -> amd -> arm`.

Production engine behavior must never branch on an application, language, runtime,
framework, executable name, build-information marker, or vendor identity.  In
particular, Go, V8, JVM, and similar guest internals are not Linux ABI
domains.  When retained C contains such a branch, preserve it as migration evidence
and identify the violated generic invariant (for example non-PIE guest-address
placement or signal semantics); repair that invariant rather than creating a
runtime-specific Rust package.  Guest-visible addresses remain ELF/Linux addresses;
host storage placement is an internal mapping detail and must not leak into guest
pointers, symbols, signals, `/proc`, checkpoints, or runtime metadata.

## C oracle study before every migration lane

Reading retained fixtures and expected output is necessary but insufficient.
Before changing a runtime domain, the lane owner must inspect the corresponding
read-only implementation in `../engine` and record:

- the exact C and assembly files and entry functions studied;
- state ownership, identity, lifetime, locking, and teardown behavior;
- syscall ordering, partial-result, blocking, cancellation, signal, and errno
  semantics relevant to the lane;
- architecture-specific and host-specific branches;
- the explicit mapping from each observed C capability to its Rust owner, or an
  honest remaining gap.

Record this oracle audit beside the relevant compatibility or performance report
before the lane is accepted.
An agent report that cites only tests, manifests, expected output, or summaries
does not satisfy this requirement. Never edit `../engine` while performing the
audit.

### Port domains, not failing cases

The retained C engine is the primary implementation oracle. Compatibility cases
are acceptance evidence and prioritization signals; they are not a substitute
for migrating the implementation that already works.

Before fixing a corpus cluster, read the complete retained C domain and its call
graph rather than only the function named by the first failure. Inventory every
entry point, state object, ownership edge, lock, wakeup, error path, architecture
branch, and teardown transition, then compare that inventory mechanically against
the Rust owners. Record a dense capability matrix with each C capability marked
implemented, divergent, or missing in Rust. Implement the largest coherent
missing mechanism and all of its widths, flags, lifecycle paths, and error
semantics before returning to the corpus.

Walking one executable until it exposes the next unsupported instruction or
patching one fixture-visible branch at a time is forbidden when the retained C
tables or domain implementation can reveal the complete family in one audit.
Likewise, a narrow passing case does not prove a domain port complete. Acceptance
requires focused cohort evidence after the implementation comparison and later a
full-corpus checkpoint from the exact committed tree.

## Source layers

The source tree separates reusable foundations, engine runtime domains, native
execution, container capabilities, workspace capabilities, and the product root:

```text
src/
  packages/   transferable libraries and repository tool packages
  runtime/    engine-specific runtime domains
  native/     CPU schema and native execution implementation
  containers/ container services and the integrated hl-engine
  workspaces/ workspace, terminal, and generic GUI capabilities
  apps/husklet/ the product composition root
```

Dependencies point inward:

```text
husklet -> workspaces + containers -> runtime -> packages -> std
                              -> native
```

- Production libraries in `packages/` must make sense without an engine, guest,
  syscall, emulator, or container.
- `runtime/` packages each own one coherent engine domain.
- `containers/hl-engine` selects concrete engine adapters and glues runtime domains together.
- `apps/husklet` selects product adapters and composes containers, workspaces, terminal, and GUI.
- No package depends on `apps/husklet`.
- Repository tools live as packages under `src/packages/`, but remain build-time
  machinery and never production dependencies. The generic `hl-design` annotation
  package is the only explicitly reviewed exception when used by production crates.

Changing a local Cargo dependency requires explaining the ownership reason and
passing the dependency linter.

### UI ownership

- `hl-gui` owns generic visual primitives, layout, validation display, accessibility,
  and toolkit adapters.
- Husklet owns screens, settings schemas, product view models, navigation, and feature
  composition.
- Generic components receive state and emit typed intent. They do not persist,
  orchestrate, or invoke services.
- Product components such as workspace pickers, image choosers, removal confirmations,
  and terminal settings stay beside the feature that owns them.
- Native toolkit types do not cross the GUI boundary.
- Add a component only for a stable concept, state contract, interaction contract,
  accessibility behavior, or cohesive reuse; keep one-off layout beside its page.

## Package placement

Ask these questions in order:

1. Is it repository-only lint, differential, fixture, or benchmark machinery
   that is forbidden as a production dependency? Put its package in `packages/`
   and keep the tool boundary explicit. Audits that understand engine-owned
   runtime domains, such as syscall admission, live in `runtime/` but remain
   forbidden as production dependencies.
2. Does the code extend ordinary logging, filesystem, byte I/O, encoding, or
   another standard-library mechanism without engine vocabulary? Put it in
   `packages/`.
3. Does it own a Linux-engine entity, lifecycle, state machine, or invariant? Put
   it in the corresponding package under `runtime/`.
4. Does it connect two runtime domains or select a concrete platform adapter? Put
   the integration in `runtime/hl-runtime`.
5. Does it validate engine configuration, expose the engine API/CLI/C ABI, or
   construct the complete engine? Put it in `containers/hl-engine`.
6. Does it own product configuration, screens, commands, navigation, or cross-domain
   composition? Put it in `apps/husklet`.

Do not add catch-all packages or modules named `core`, `common`, `shared`, `types`,
`utils`, `helpers`, or `misc`. Name code by the entity, capability, algorithm, or
external mechanism it owns.

Do not create an outer directory containing one crate. The three source layers are
the meaningful grouping. Runtime concepts such as ISA, memory, networking, tasks,
and execution are sibling packages under `src/runtime/`.

## Domain ownership

Each runtime package owns:

- its entities and value types;
- valid-state construction;
- lifecycle and concurrency invariants;
- domain operations and typed errors;
- consumer-owned capability traits;
- pointer-free, bounded snapshot values;
- platform adapters only when the mechanism belongs solely to that domain.

Each domain exposes a small public surface from its crate root. Other packages must
not import private modules or reproduce its models.

Cross-domain operations live in `hl-runtime`:

| Operation | Domains joined |
|---|---|
| file-backed mapping | descriptor + VFS + memory |
| procfs | VFS + task |
| signalfd | event + task + descriptor |
| Unix pathname socket | VFS + network |
| `SCM_RIGHTS` | network + descriptor |
| fork | task + descriptor + memory + execution |
| exec | task + loader + descriptor + memory |
| provider-backed object | provider + receiving domain |
| syscall trap | execution + Linux personality |
| checkpoint | all snapshot-capable domains |

These adapters use public APIs and owned values. They never access private fields.

## Ports and adapters

A port is a narrow trait owned by the consumer that needs the capability. Add a
port only for a real platform, substitution, testing, FFI, or stable domain
boundary.

Examples:

- task owns `GuestExecutor`; execution implements it;
- execution owns `TrapHandler` and `InstructionMemory`; runtime implements them;
- memory owns `Backing`; runtime adapts a pinned open-file description;
- VFS owns `VfsHost`; the app supplies the selected host adapter;
- network owns `SocketHost`; the app supplies the selected host adapter.

Never introduce a shared `host-api`, service locator, or omnibus platform trait.
Keep traits small and capability-specific.

## Native execution boundary

The retained C/assembly kernel lives under `src/native/execution`. It is limited to:

- CPU layouts whose offsets are embedded in machine code;
- assembly entry, block-return, and trampoline code;
- W^X code-cache mutation, publication, lookup, and chaining;
- POSIX signal/ucontext and Windows VEH/CONTEXT entry;
- fault-context reconstruction;
- async-signal-safe and fork-critical repair.

It must not own Linux syscall, filesystem, descriptor, networking, task, loader,
checkpoint, or product policy.

Cross-language operations are coarse. FFI per instruction, guest memory access,
block lookup, or chain transition is forbidden.

CPU layouts are generated from `src/schema/cpu` into C and Rust. Both sides compile
size, alignment, and offset assertions. Hand-maintained duplicate layouts are
forbidden.

## Unsafe code

Workspace code forbids unsafe by default.

Unsafe is permitted only in reviewed modules that implement:

- platform system calls;
- the native execution ABI;
- the external C ABI;
- memory mapping and fault entry that cannot be expressed safely.

Every unsafe block states:

1. the validity, lifetime, alignment, and aliasing assumptions;
2. which owner keeps referenced storage alive;
3. why concurrent access is valid;
4. why failure cannot unwind across FFI.

No allocation, lock acquisition, logging, panic, unwinding, or Rust destructor walk
may occur in a signal, VEH, or fork-critical callback.

## Types and ownership

- Make invalid states unrepresentable with constructors, enums, and meaningful
  newtypes.
- Do not wrap primitives or collections without an invariant, identity boundary, or
  cohesive behavior.
- Borrow for observation and transfer ownership for storage.
- Clone only when the ownership model requires independent ownership.
- Use checked arithmetic where overflow is invalid and saturating arithmetic only
  where clamping is the contract.
- Guest-provided lengths, counts, offsets, command batches, and resource requests
  must be bounded before allocation or expensive host work.
- A descriptor, OFD, mapping, task, subscription, provider handle, and translated
  block each have one explicit owner and generation/lifetime model.
- Do not use process-global mutable state for engine instances.

## Errors and Linux behavior

Libraries return typed domain errors. Linux errno conversion happens at the Linux
personality boundary.

Preserve:

- exact `EAGAIN`, `EWOULDBLOCK`, `EINTR`, and partial-I/O behavior;
- shared OFD offsets and descriptor-local flags;
- epoll edge, level, oneshot, timeout, cancellation, and wakeup ordering;
- `SCM_RIGHTS` ownership;
- shared mapping visibility and protection ordering;
- futex deadlines and wakeups;
- fork/exec descriptor, signal, task, and mapping transitions.

Do not panic for guest input or recoverable host failures. No panic or unwind may
cross a C boundary.

## Concurrency and performance

- Avoid global locks across unrelated engines, processes, descriptors, mappings, or
  translated blocks.
- Do not hold table locks across host calls.
- Unrelated OFDs must not serialize.
- Define task ownership, cancellation, shutdown, and wakeup ordering.
- Backpressure blocks or rejects predictably; it never busy-spins.
- Do not log every syscall or translated instruction in normal operation.
- Do not introduce synchronous full-frame, device-wide, or whole-engine waits in a
  hot path.
- Preserve explicit bounds for caches, commands, memory, threads, handles, logs, and
  retained resources.

Every hot-path migration compares against a pinned C baseline. Nested engine
benchmarks measure compounding overhead.

## Application boundaries

`src/containers/hl-engine` is the engine composition root. It owns:

- public configuration and validation;
- CLI and environment capture;
- platform and execution-backend selection;
- concrete adapter construction;
- the supported Rust API;
- the opaque C ABI;
- packaging and target-specific linkage.

The engine wires capabilities and delegates behavior. It must not become the owner of
filesystem, descriptor, syscall, task, or execution algorithms.

`src/apps/husklet` is the product composition root. It owns product configuration,
GUI/CLI behavior, backend selection, and cross-domain orchestration. It must delegate
container, workspace, terminal, filesystem, and engine behavior to their owners rather
than becoming a service locator or god object.

## Tests

- Unit tests live beside the owning source.
- Crate `tests/` exercise only that crate's public contract.
- Repository `tests/` contains multi-package, process, hardware, application, and
  engine-in-engine tests.
- Tests are deterministic, isolated, bounded, and responsible for their resources.
- Fixes begin with a failing behavioral test when feasible.
- Differential tests run the same operation against C and Rust and compare results,
  errno, state, ownership, ordering, and serialized data.

A directory under `src/` must not exist only to aggregate detached test fragments.
When two or more Rust files in a source directory are all test-only, move each test
beside the production noun it exercises and prefer an inline `#[cfg(test)]` module.
Test code must not import behavior or fixtures from a sibling test module. Put
genuinely shared, behavior-free fixtures behind one explicitly declared
`test_support` module owned by the production boundary instead.

Required migration gates are:

1. formatting, design lint, Clippy with warnings denied, unit and documentation
   tests;
2. C/Rust ABI and differential tests;
3. both guest ISA compatibility and production tests;
4. checkpoint and cross-checkpoint;
5. native ARM64 macOS/Linux, AMD64 Linux, and AMD64 Windows target checks;
6. nested engine and performance tests;
7. ordinary container and interactive terminal workflows through Husklet.

### Reproducible Nix driver

`flake.lock` pins the development and verification toolchain. Use the flake as
the repository-level entry point:

```text
nix develop
nix build -L --option cores 0 --max-jobs auto
nix flake check -L --option cores 0 --max-jobs auto
```

Run Clippy and rustfmt only through `make lint` (alias of `make clippy`), `make fmt`,
`make fmt-check`, or `make gate`; each enters the pinned shell. A bare `cargo clippy`
on a host whose `cargo`/`rustc` come from a distribution package but whose
`clippy-driver` comes from Nix fails with `error[E0514]: found crate ... compiled by
an incompatible version of rustc` even though both report the same version string,
because the two builds hash crate metadata differently.

The default shell exposes both Linux guest compilers and the retained
`*_LINUX_CC`, `*_LINUX_STATIC_CC`, `*_DYNAMIC_LOADER`, and `*_DYNAMIC_LIBC`
contracts. Interactive verification must override conservative environment
defaults and size `CARGO_BUILD_JOBS` and `HL_COMPAT_JOBS` to the host's logical
CPU count unless measured RAM, disk, thermal, or lifecycle pressure requires a
lower bound. The named flake checks alias one comprehensive verification
derivation deliberately; use its internal parallelism rather than launching
duplicate full Cargo builds that contend for the same dependency graph.
The derivation must remain offline, locked, warning-strict, and responsible for
format, design lint, lint cases, workspace and documentation tests, and checked
compatibility metadata. Do not reintroduce retained-tree CMake, Ninja, clang, or
cppcheck dependencies unless Rust-owned build code actually requires them.

## Design lint

`src/packages/hl-design-lint` is the repository architecture linter. Run:

```text
make design-lint
make lint-cases
```

It enforces dependency direction and cycles, source ownership, ambient environment
access, platform-command boundaries, catch-all modules, oversized files, ceremonial
structure, and other reviewed design rules.

`lint/errors/` contains unclassified generated findings. `lint/check/` contains
temporarily classified findings. Both are review queues, not suppressions.

`lint/examples/positive.md` contains approved transformations.
`lint/examples/negative.md` contains rejected transformations and their failure
modes. The corpus began from Husklet's reviewed examples; engine-specific decisions
must be added as the rewrite exposes real cases.

### Lint-case protocol

Before resolving a generated lint case, read:

- this entire `AGENTS.md`;
- all of `lint/examples/positive.md`;
- all of `lint/examples/negative.md`;
- the current source, callers, sibling behavior, owning manifest, and nearby tests.

A generated case is evidence and may be stale. Refactor into the correct entity,
package, port, adapter, or inline behavior when ownership is clear. Do not add a
classification, allowance, dependency, wrapper, or empty abstraction merely to
make the queue pass.

Append a positive or negative example only after user approval. Preserve the
reasoning, not only the final code.

## Style

- Use precise nouns and domain vocabulary.
- Avoid `Manager`, `Helper`, `Util`, `Impl`, vague abbreviations, and repeated
  module prefixes.
- A trait or type is already a namespace; method names do not repeat it.
- Prefer standard conversion, parsing, formatting, and iterator traits when they
  express the complete contract.
- Keep the happy path shallow.
- Public APIs are minimal and document invariants, errors, safety, ownership, and
  non-obvious performance contracts.
- Comments explain contracts and reasons; names explain mechanics.
- Lint allowances are local and justified.
- Delete obsolete implementations after their migration and parity window passes.

## Delivery

Refactor incrementally. Every migration leaves an acyclic package graph and a
working production path. Temporary dependency cycles, permanent compatibility
shims, application-specific engine hacks, and parallel abandoned implementations
are not accepted migration strategies.
