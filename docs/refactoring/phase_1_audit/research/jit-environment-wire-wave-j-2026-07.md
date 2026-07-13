# JIT environment and launch-wire audit — wave J (2026-07)

Scope: all environment names read by tracked JIT C, Rust `SpawnConfig` forwarding, typed `LaunchConfig`, CLI/daemon/test producers, persistent-cache identity, documentation, and every launch-wire field. No code was changed.

## Result

The C runtime has 128 literal `getenv` names. The legacy `SpawnConfig::script` mac bridge explicitly forwards 29 ambient tuning/debug names (including `CRASHDBG`); caller-supplied `SpawnConfig.env` can additionally set arbitrary names. The production `Runtime` path is different: it converts a known container configuration into an environment-free typed wire and does **not** carry arbitrary engine tuning. Thus a C reader can be reachable in a direct engine or legacy-shell launch yet impossible to select through the normal typed API.

No explicit legacy-forwarding entry lacks a C reader. The stale surface is semantic instead: several forwarded flags have no tracked CLI, daemon, or test producer and exist only because the forwarding list copied old experiments (`IBPROF`, `MAPDUMP`, `T2DUMP`, `NOSTEAL*`, `NOIBSLIM`, `NOSMCHASH`, and others). Wave D/G contains their behavioral disposition.

The typed wire has no accidentally lost payload: all fields except `reserved0` are consumed by `ddjit_configfd.c`; `reserved0` is a documented alignment/future-extension pad. The serializer’s offsets and C header agree. There is therefore no behavior-neutral live wire-field deletion. Removing `reserved0` alone would change the 128-byte ABI layout for no benefit and should not be done.

## End-to-end classes

### Typed production controls (complete path)

Rust builder/config → wire → `ddjit_configfd.c` → C reader is present for rootfs, lowers, hostname, memory/pids/CPU limits, read-only root, uid/gid, sandbox, network isolation, publish-daemon ownership, network id/IP, filesystem generation file, SOCKS egress, publishes, volumes, ulimits, netns, cwd, guest environment, pcache directory, per-container `nopcache`, GPU IOSurface, and argv.

The wire rehydrates these established C names: `DD_MEM_MAX`, `DD_PIDS_MAX`, `DD_CPUS`, `DD_ROOTFS_RO`, `DD_NET_ISOLATE`, `DD_PUBLISH_DAEMON`, `DD_GPU_IOSURFACE`, `DD_UID`, `DD_GID`, `DD_HOSTNAME`, `DD_ULIMITS`, `DD_PUBLISH`, `DD_LOWER`, `DD_NETNS`, `DDVOL`, `DD_CWD`, `DD_GUEST_ENV`, `DD_NETBR`, `DD_IP`, `DD_FSGEN_FILE`, `DD_EGRESS_SOCKS`, `DDJIT_PCACHE`, `DDJIT_PCACHE_DIR`, `DDJIT_NOPCACHE`, `DDJIT_UNTRUSTED`, and `DDJIT_SANDBOX`.

These are internal adapter variables, not ambient API: typed callers set fields and the engine locally reconstructs names. Their off-state cost is generally one initialization-time `getenv`; GPU/network/filesystem features additionally retain cached branches in their relevant syscall/VFS paths. They are behavior-bearing and not deletion candidates.

### Legacy ambient forwarding

The bridge forwards `CRASHDBG`, pcache controls, `COLDPROF`, `JT`, `JTS`, `DD_NOPATHCACHE`, `W4_NOOPENCACHE`, `DDDBG_GPRDUMP`, `DDDBG_NOCHAIN`, `MAPDUMP`, `NOSTITCH`, `NOLSE`, `NOSMC`, `NOSMCHASH`, `NOFUTEXQ`, `NOIBSLIM`, `NOMTIBTC`, `NOSTWRECLAIM`, `IBPROF`, `NOSTEAL1617`, `NOSTEALFAST`, `NOSHADOWTUNE`, `SHADOWGATE`, `PROF`, `T2DUMP`, and ARM `NOTIER2`. Every name has a reader.

`SpawnConfig.env` is also appended, so every other C reader can be selected by a deliberate legacy caller. This means “not in the explicit list” is not dead code. It means ambient host selection is unavailable unless the caller knows and injects the private name.

Behavior-neutral deletion from this list is limited to flags whose C feature is deleted simultaneously. Removing forwarding alone silently breaks an external/debug ABI. In particular, delete the ARM-B1 entries as wave D group D1, remove their readers and docs in the same commit, and then shrink this list. Do not independently delete forwarding for `NOSMC`, `NOFUTEXQ`, `NOSTWRECLAIM`, `NOMTIBTC`, `NOLSE`, tracing, or pcache controls.

### C readers unavailable through typed launch

The normal typed API cannot express `JT`/`JTS`, profiling, crash/debug dumpers, fault injection, translator fallbacks, tier thresholds, checkpoint/restore directories, or most developer cache diagnostics. This includes tested `DDJIT_NOFASTSYS`: Rust tests that inject it use a launch route capable of environment injection, but `Runtime`/`LaunchConfig` has no typed fast-syscall fallback field.

This is not necessarily a bug. The typed API promises container configuration rather than an arbitrary host environment. Classify readers as:

- supported operator policy: pcache and sandbox already have typed fields/default translation;
- emergency compatibility: if it must work in production (`DDJIT_NOFASTSYS`, `NOSMC`, perhaps checkpoint/restore), add a narrow typed enum/boolean, not `Vec<(String,String)>`;
- developer diagnostics: keep direct-engine/test injection, with no typed production field;
- orphaned experiment: delete reader, forwarding, pcache identity and docs together.

Adding generic tuning environment to the wire would destroy the env-free contract and make cache identity/auditing harder.

## Name and architecture mismatches

`DD_LOWER` and `DD_LOWERS` are two real dialects, not a current typo. Linux engines consume colon-separated `DD_LOWER`; the typed wire joins lowers with `:` and rehydrates `DD_LOWER`. Darwin jail consumes comma-separated `DD_LOWERS`; legacy `SpawnConfig` emits that name with commas. Docs that imply either name is universal are stale and should describe the target-specific adapter boundary. Do not merge them without changing both separators and compatibility.

Other architecture splits remain misleading:

- forwarded `NOTIER2` controls ARM only; x86 reads unforwarded `NOTIER2X`;
- `DDDBG_GPRDUMP`/`DDDBG_NOCHAIN` are forwarded generically but their shared x86 variables are inert;
- ARM-only `IBPROF`, `NOIBSLIM`, `NOSTEAL*`, `NOSHADOWTUNE`, and `SHADOWGATE` are forwarded for either guest selection;
- x86-only `NOEAOPT`, `NOSSEOPT`, `NOX87OPT`, flag-elision gates, `NOTIER2X`, and tier threshold are not in the ambient forwarding list.

These checks cost at most process/translation-time branches in the wrong architecture, but they expand a false cross-architecture ABI. When a feature remains supported, expose it with architecture validation; when it is diagnostic, stop describing the forwarding array as engine-agnostic.

## Persistent-cache contract

Environment controls that alter emitted bytes must be represented in pcache identity or poison persistence. ARM hashes its principal codegen variables and poisons experimental pointer-bearing modes. The x86 omissions identified in wave G remain: `DDJIT_NOSLIMSYS`, `DDJIT_NOFASTSYS`, `NOTIER2X`, several flag-elision masters, and `NOX87OPT` can change emitted code without a mode bit. A name’s presence in forwarding does not fix this. Until deletion/keying, tests must use pcache off or isolated directories.

Container-only names and runtime behavior controls need no codegen bit. Conversely, deleting an environment reader requires deleting its stale mode bit/hash input; leaving it creates distinct cache files for behavior-identical runs.

## Off-state cost and neutral cleanup

Most configuration readers run once and cache their result. The material disabled costs are already isolated in prior waves: IBPROF reserves about 60.9 MiB BSS; MAPDUMP carries Mach watcher/snapshot text and performs run/exit lookups; BLKDUMP/T2DUMP add translation-time checks; A/B flags add translation-time branches. Container feature gates can branch in relevant syscall/VFS paths but are active product capabilities.

Exact behavior-neutral cleanup available now:

1. After deleting an orphaned feature, delete its legacy-forwarding string, architecture-inert compatibility global/hook, pcache mode entry, inventory row, and test workload claim in the same change.
2. Correct docs to distinguish Linux `DD_LOWER` (`:`) from Darwin `DD_LOWERS` (`,`); no runtime rename is required.
3. Remove architecture-inert forwarding only if the launch code validates guest type and still forwards the name for its live architecture; this changes no engine behavior but may change generated shell text, so keep script snapshot tests updated.
4. Do not remove `reserved0`, container adapter variables, or arbitrary `SpawnConfig.env`; those are wire ABI or documented legacy/debug escape surfaces.

## Wire audit

`mem_max`, `pids_max`, `cpus`, `uid`, `gid`, `rootfs_ro`, `sandbox`, `net_isolate`, `publish_daemon`, every string offset, `gpu_iosurface`, `nopcache`, and `egress_off` are read. `rootfs_off` reaches `dd_run`; `argv_off` is validated and rebuilt. `header_len` permits tail skew and the reader discards unknown new tail bytes. `reserved0` alone is intentionally unread.

Acceptance for future wire deletion/addition: Rust layout/offset tests, a C `_Static_assert` on size/offsets, old-writer/new-reader and new-writer/old-reader tail-skew tests, typed launch integration for every non-default field, and a grep proving every serialized field is either consumed or explicitly reserved.

## Recommended consolidation gate

Generate the environment inventory during Rust/C tests from a checked-in manifest with columns: name, architecture, owner, launch class (typed/legacy/direct), producer, pcache effect, and stability (product/emergency/debug/experiment). Fail when a literal C reader or explicit forwarding string is absent. This prevents the current hand-maintained forwarding and rebrand inventories from drifting without adding source-text “implementation exists” tests; behavioral tests remain required for each supported control.
