# dd → husklet rebrand inventory: `dd-jit/` + `dd-jit-darwin/`

READ-ONLY map, produced 2026-07-07. Ground truth for a later, mechanical rename of the `dd`
brand prefix. **Scope: only `dd-jit/` (public runtime API crate) and `dd-jit-darwin/` (engine
backend).** Other crates (`dd-cli`, `dd-daemon`, `dd-client`, `dd-gpu`, `dd-display`, `dd-gui`,
`dd-images`, `dd-tests`, `dd-term-core`) are **NOT covered here** — a later pass must extend this;
where they set/read an env var or ABI symbol that crosses into these two crates it is flagged in
§Risk notes.

Nothing was renamed. All `file:line` refs are real.

---

## Summary counts

| Category | Count |
|---|---|
| Env vars read/written by the engine+runtime | **131** distinct names (see §1) |
| — of which `DD_`/`DDVOL` container-config class | 32 |
| — `DDJIT_` engine class | 17 |
| — `DDDBG_` / other bare-`DD` debug class | 9 |
| — bare-control (NO dd prefix) internal flags | 69 |
| — cargo compile-time (rustc-env) branded | 4 |
| Branded C symbols (`dd_*`, `ddjit_*`, `dd*`-structs) | ~40 fns + ~14 structs/types + macro families |
| `DDJIT_*` / `DD_*` C macros & constants (non-env) | see §2 |
| Names: crates / lib targets / bins / artifacts / sonames | see §3 |
| On-disk paths, xattr keys, mach service, brand log strings | see §3 |

### Rename-mapping proposal (legend — CONFIRM with user before executing)

| From | To (proposed) | Note |
|---|---|---|
| env `DD_*` | `HL_*` | container-config; user-/cross-crate-facing → MUST rename in lockstep |
| env `DDVOL` | `HL_VOL` | odd one-off (no underscore); align to `HL_VOLUMES`? user to decide |
| env `DDJIT_*` | `HL_*` or `HLJIT_*` | engine tuning; ambiguity: keep a `JIT` infix or collapse to `HL_`? |
| env `DDDBG_*` | `HLDBG_*` | debug; low value to rename (see risk notes) |
| env `DDEPOLLPROF`/`DDRELRODYN`/`DDFAILMMAP`/`DDFAILMPROT` | keep or `HL…` | internal-only debug; likely KEEP |
| bare-control flags (`JT`, `NOSMC`, `PROF`, `T2DUMP`, `NOXAL*`, …) | **KEEP** | not dd-branded; internal A/B kill-switches |
| C `ddjit_*` (ABI: `ddjit_spawn`, `ddjit_config`, `ddjit_run_configfd`) | `hl_*` / `husklet_*` | **ABI — rename in lockstep with Rust FFI** |
| C `dd_*` fns (`dd_run`, `dd_lock`, `dd_gpu_*`, …) | `hl_*` | internal C, safe as a block rename |
| C macro `DDJIT_CONFIG_MAGIC` etc. | `HL_*` | mirrored in Rust wire.rs — rename BOTH |
| Rust crate `dd-jit` | `husklet-jit`? | lib target `dd_jit` → `husklet_jit` |
| Rust crate `dd-jit-darwin` | `husklet-jit-darwin`? | lib target `dd_jit_darwin` |
| artifacts `ddjit-<target>`, `darwinjail.dylib`, `libddjit_ffi.a` | `hl-*` / keep darwinjail? | resolved at runtime by name — rename baker+resolver together |
| default hostname `"jit"` | `"husklet"`? | user-visible inside container (uname/hostname) |
| xattr `user.dd.uid`/`user.dd.gid`, `user.ddx.` | `user.hl.*` | **ON-DISK persisted** — migration concern, see risk notes |
| mach service `com.dd.display.gpu` | `com.husklet.display.gpu` | cross-crate with dd-display — lockstep |

Ambiguities the user must resolve: (a) does `DDJIT_` collapse to `HL_` or keep a JIT infix;
(b) `DDVOL` naming; (c) whether internal debug flags rename at all; (d) on-disk xattr/path
migration for already-created containers.

---

## §1. ENVIRONMENT VARIABLES (the priority)

Read = `getenv` (C) / `env::var` (Rust). Write = `setenv` (C) / `.env()` (Rust Command) /
`cargo:rustc-env` (build). All files under `dd-jit-darwin/src/` unless the path says `dd-jit/`.

### 1a. `DD_*` + `DDVOL` — container-config class (USER / CROSS-CRATE facing → rename in lockstep)

| Name | Read sites | Write / inject sites | Purpose | Target |
|---|---|---|---|---|
| `DD_CPUS` | os/darwin/jail/jail.c:260, os/linux/container/state.c:659 | os/ddjit_configfd.c:84 | online-CPU count to advertise | `HL_CPUS` |
| `DD_CWD` | jail.c:237, os/linux/forkserver.c:311, targets/linux_aarch64.c:409, targets/linux_x86_64.c:170 | ddjit_configfd.c:116 | initial cwd inside container | `HL_CWD` |
| `DD_EGRESS_SOCKS` | os/linux/container/netns.c:1285 | — | egress SOCKS proxy addr | `HL_EGRESS_SOCKS` |
| `DD_FAULTCOUNT` | targets/linux_aarch64.c:447,716 | — | fault-count debug gate | `HL_FAULTCOUNT` |
| `DD_FORKPROF` | os/linux/syscall/proc.c:170 | — | fork-path profiling | `HL_FORKPROF` |
| `DD_FSGEN_FILE` | os/linux/fscache.c:769 | ddjit_configfd.c:124 | shared external-writer generation file | `HL_FSGEN_FILE` |
| `DD_GID` | targets/linux_aarch64.c:380, targets/linux_x86_64.c:123 | ddjit_configfd.c:99 | run gid | `HL_GID` |
| `DD_GPU_IOSURFACE` | os/linux/container/vfs.c:112 | ddjit_configfd.c:92 | opt-in host-IOSurface GPU path | `HL_GPU_IOSURFACE` |
| `DD_GUEST_ENV` | vfs.c:910,1382, os/linux/elf.c:817, translate/x86_64/elf.c:424 | ddjit_configfd.c:118, os/linux/syscall/proc.c:42 | `\n`-joined guest env | `HL_GUEST_ENV` |
| `DD_HOSTNAME` | jail.c:229, vfs.c:1560, targets/linux_aarch64.c:340 | ddjit_configfd.c:104 | UTS hostname | `HL_HOSTNAME` |
| `DD_IP` | netns.c:615, state.c:162 | ddjit_configfd.c:122 | container IP on user switch | `HL_IP` |
| `DD_LOWER` | targets/linux_aarch64.c:350, targets/linux_x86_64.c:158 | ddjit_configfd.c:110 | `:`-joined overlay lowers (linux) | `HL_LOWER` |
| `DD_LOWERS` | jail.c:231 | — | overlay lowers (darwinjail) | `HL_LOWERS` |
| `DD_MEM_MAX` | jail.c:244, targets/linux_aarch64.c:343 | ddjit_configfd.c:76 | cgroup memory.max bytes | `HL_MEM_MAX` |
| `DD_NETBR` | netns.c:610 | ddjit_configfd.c:120 | user-network virtual-switch id | `HL_NETBR` |
| `DD_NET_ISOLATE` | jail.c:277, netns.c:2026, state.c:147, os/linux/syscall/net.c:544 | ddjit_configfd.c:87 | `--network none` | `HL_NET_ISOLATE` |
| `DD_NETNS` | netns.c:1472, vfs.c:1559, os/linux/syscall/helpers.c:513, os/linux/syscall/sysv.c:299, targets/linux_aarch64.c:367,375, targets/linux_x86_64.c:134 | targets/linux_x86_64.c:141, targets/linux_aarch64.c:376, ddjit_configfd.c:112 | private-loopback netns key | `HL_NETNS` |
| `DD_NONETNS` | targets/linux_aarch64.c:366, targets/linux_x86_64.c:128 | — | disable private netns | `HL_NONETNS` |
| `DD_NOPATHCACHE` | os/linux/fscache.c:384 | — | path-resolution cache kill switch | `HL_NOPATHCACHE` |
| `DD_PCACHE` | **dd-jit/src/runtime/runtime.rs:30** | — | Rust-side pcache default toggle (`!=0`) | `HL_PCACHE` |
| `DD_PID1` | jail.c:230 | — | darwinjail pid-1 marker | `HL_PID1` |
| `DD_PIDS_MAX` | jail.c:244, targets/linux_aarch64.c:345 | ddjit_configfd.c:80 | pids.max | `HL_PIDS_MAX` |
| `DD_PUBLISH` | jail.c:233, targets/linux_aarch64.c:348, targets/linux_x86_64.c:154 | targets/linux_x86_64.c:462, ddjit_configfd.c:108 | published ports `H:C,…` | `HL_PUBLISH` |
| `DD_PUBLISH_DAEMON` | netns.c:892 | ddjit_configfd.c:88 | external forwarder owns ports | `HL_PUBLISH_DAEMON` |
| `DD_ROOTFS` | jail.c:228 | — | container rootfs (darwinjail) | `HL_ROOTFS` |
| `DD_ROOTFS_RO` | jail.c:258, state.c:664 | ddjit_configfd.c:86 | rootfs/upper read-only | `HL_ROOTFS_RO` |
| `DD_SANDBOX` | jail.c:267; **dd-jit/src/runtime/runtime.rs:32** | — | sandbox default (darwinjail + Rust default) | `HL_SANDBOX` |
| `DD_SIGURG` | os/linux/signal.c:43 | — | SIGURG handling toggle | `HL_SIGURG` |
| `DD_UID` | targets/linux_aarch64.c:378, targets/linux_x86_64.c:121 | ddjit_configfd.c:95 | run uid | `HL_UID` |
| `DD_ULIMITS` | jail.c:266, state.c:665 | ddjit_configfd.c:106 | `name=soft:hard,…` | `HL_ULIMITS` |
| `DD_VOLUMES` | jail.c:232 | — | volumes (darwinjail) | `HL_VOLUMES` |
| `DDVOL` | targets/linux_aarch64.c:399, targets/linux_x86_64.c:144 | ddjit_configfd.c:114 | volumes (linux engine) — **note: no underscore** | `HL_VOL` (confirm) |

### 1b. `DDJIT_*` — engine class (tuning, pcache, checkpoint, sandbox gates)

| Name | Read sites | Write sites | Purpose | Target |
|---|---|---|---|---|
| `DDJIT_CHECKPOINT_DIR` | os/linux/checkpoint.c:178 | — | CRIU-style checkpoint dir | `HL_CHECKPOINT_DIR` |
| `DDJIT_CKPT_DEBUG` | checkpoint.c:870,914 | — | checkpoint debug logging | `HL_CKPT_DEBUG` |
| `DDJIT_DIR` | **dd-jit-darwin/src/guest.rs:90** | — | override dir to locate `ddjit-*` engine artifacts | `HL_DIR` |
| `DDJIT_FASTSTAT` | targets/linux_x86_64.c:351 | — | fast-stat syscall path | `HL_FASTSTAT` |
| `DDJIT_FASTSYS_FORCE` | translate/x86_64/emit.c:736 | — | force fast-syscall inline | `HL_FASTSYS_FORCE` |
| `DDJIT_NOFASTHRM` | os/linux/thread.c:338 | — | disable fast thread-create | `HL_NOFASTHRM` |
| `DDJIT_NOFASTSYS` | emit.c:717 | — | disable fast-syscall path | `HL_NOFASTSYS` |
| `DDJIT_NOPCACHE` | targets/linux_aarch64.c:639,651,654, targets/linux_x86_64.c:365 | os/linux/forkserver.c:330; **dd-jit/src/runtime/container/mod.rs:215** (`.env`) | disable persistent code cache | `HL_NOPCACHE` |
| `DDJIT_NOSIGINLINE` | emit.c:722 | — | disable inlined signal check | `HL_NOSIGINLINE` |
| `DDJIT_NOSLIMSYS` | engine/stubs.c:65, emit.c:623 | — | disable slim-syscall path | `HL_NOSLIMSYS` |
| `DDJIT_NOXATTRCACHE` | state.c:290 | — | disable xattr cache | `HL_NOXATTRCACHE` |
| `DDJIT_PCACHE` | targets/linux_aarch64.c:639,654, targets/linux_x86_64.c:365 | ddjit_configfd.c:129; forwarded spawn_config.rs:157 | enable persistent code cache | `HL_PCACHE` |
| `DDJIT_PCACHE_DIR` | translate/aarch64/pcache.c:268, translate/x86_64/pcache.c:197; **runtime.rs:31** | ddjit_configfd.c:130; spawn_config.rs:158 | pcache directory | `HL_PCACHE_DIR` |
| `DDJIT_RESTORE_DIR` | checkpoint.c:177, targets/linux_aarch64.c:646,820 | targets/linux_aarch64.c:787 | checkpoint restore dir | `HL_RESTORE_DIR` |
| `DDJIT_SANDBOX` | targets/linux_aarch64.c:545, targets/linux_x86_64.c:273 | ddjit_configfd.c:135 | untrusted-guest sentry gate | `HL_SANDBOX` (collides w/ `DD_SANDBOX`→`HL_SANDBOX`; disambiguate) |
| `DDJIT_UNTRUSTED` | targets/linux_aarch64.c:544, targets/linux_x86_64.c:272 | ddjit_configfd.c:134 | untrusted-guest gate | `HL_UNTRUSTED` |
| `DDJITD_DIAG` | os/linux/forkserver.c:313,331 | — | forkserver-daemon diagnostics | `HLD_DIAG` |

> ⚠ Naming collision: both `DD_SANDBOX` and `DDJIT_SANDBOX` naïvely map to `HL_SANDBOX`. They are
> distinct vars today (darwinjail vs. linux sentry). Pick distinct targets.

### 1c. `DDDBG_*` and other bare-`DD` debug vars (internal — probably KEEP or `HLDBG_*`)

| Name | Read sites | Purpose |
|---|---|---|
| `DDDBG_DROPURG` | os/linux/signal.c:48 | drop SIGURG for debugging |
| `DDDBG_GPRDUMP` | targets/linux_aarch64.c:520 (forwarded spawn_config.rs:161) | dump GPRs |
| `DDDBG_IMGBASE` | targets/linux_aarch64.c:580 | pin image base |
| `DDDBG_INTERPBASE` | targets/linux_aarch64.c:597 | pin interp base |
| `DDDBG_NOCHAIN` | targets/linux_aarch64.c:519 (forwarded spawn_config.rs:162) | disable block chaining |
| `DDEPOLLPROF` | os/linux/syscall/helpers.c:947 | epoll profiling |
| `DDRELRODYN` | translate/x86_64/elf.c:355 | RELRO/dynamic debug gate |
| `DDFAILMMAP` | os/linux/elf.c:521 (via `elf_inject_fail`, elf.c:548) | fault-inject mmap failures |
| `DDFAILMPROT` | os/linux/elf.c (via `elf_inject_fail`) | fault-inject mprotect failures |

### 1d. Bare-control vars — NO dd prefix (internal A/B kill-switches, profiling, tracing → KEEP)

These carry **no dd branding** and should NOT be renamed. Listed for completeness (grep `getenv`).
A subset is force-forwarded through the `mac` bridge by `dd-jit-darwin/src/spawn_config.rs:163-181`
and `dd-jit/src/runtime/container/mod.rs:213-215`.

`AVXTRACE` (x86_64/avx.c:284), `BLKDUMP` (x86_64/translate.c:3567), `COLDPROF` (linux_aarch64.c:655,
linux_x86_64.c:364; forwarded), `CRASHDBG` (os/linux/elf.c:521, proc.c:223, thread.c:338,
linux_aarch64.c:423, x86_64/avx.c:928,1924, x86_64/elf.c:842,887, x86_64/translate.c:975; +
spawn_config.rs:147, container/mod.rs:213), `CTXDISP` (linux_aarch64.c:532),
`EXITSTAT` (x86_64/avx.c:31), `IBPROF` (linux_aarch64.c:522), `IBTC1WAY` (x86_64/engine_glue.c:51,54),
`JT` (os/linux/elf.c:594,760, linux_aarch64.c:517, linux_x86_64.c:211, x86_64/elf.c:242,380; +
elf.c:595; forwarded), `JTS` (linux_aarch64.c:518, linux_x86_64.c:212; forwarded),
`LAZYBUDGET` (x86_64/elf.c:559), `LAZYDIAG` (x86_64/elf.c:572), `MAPDUMP` (engine/cache.c:557;
forwarded), `NODUALMAP` (linux_aarch64.c:497, linux_x86_64.c:192), `NOEAOPT` (engine_glue.c:135,138),
`NOEPOLLOPT` (helpers.c:935), `NOFLAGELIDE` (shift.c:18, trace.c:217,518),
`NOFUTEXQ` (linux_aarch64.c:542), `NOGOREBASE` (x86_64/elf.c:319),
`NOGUESTFOLD` (linux_aarch64.c:540, engine_glue.c:153), `NOIBSLIM` (linux_aarch64.c:531; forwarded),
`NOIRQCHECK` (linux_aarch64.c:536, linux_x86_64.c:217, aarch64/translate.c:1242,
x86_64/translate.c:997), `NOIRQSLIM` (linux_aarch64.c:536, linux_x86_64.c:217),
`NOLAZY` (x86_64/translate.c:396), `NOLAZYFIX` (x86_64/elf.c:567),
`NOLSE` (aarch64/translate.c:1330; forwarded), `NOMTIBTC` (linux_aarch64.c:541),
`NONPIE_NOFIXUP` (x86_64/elf.c:304), `NOPFAFELIM` (x86_64/translate.c:509),
`NORELRO` (x86_64/elf.c:355), `NOREP` (repstr.c:10), `NOREPCMP` (engine_glue.c:31),
`NORWXFIX` (os/linux/syscall/mem.c:445), `NOSHADOWTUNE` (aarch64/translate.c:603),
`NOSHIFTFLAGELIDE` (shift.c:18), `NOSMC` (aarch64/translate.c:1139, x86_64/dispatch_hooks.h:52;
forwarded), `NOSMCHASH` (engine/cache.c:217; forwarded), `NOSOCKADDR` (netns.c:1205),
`NOSSEOPT` (engine_glue.c:125,128), `NOSTEAL1617` (linux_aarch64.c:527),
`NOSTEALFAST` (aarch64/translate.c:130), `NOSTITCH` (aarch64/translate.c:1313,
x86_64/translate.c:1028; forwarded), `NOTIER2` (engine/cache.c:714; forwarded),
`NOTIER2X` (linux_x86_64.c:219, engine_glue.c:182), `NOTMPFS` (vfs.c:299),
`NOV8BLOB` (x86_64/elf.c:327), `NOX87OPT` (x87.c:25), `NOXALUDIRECT` (x86_64/translate.c:411),
`NOXALUFLAGELIDE` (trace.c:228), `NOXBLOCKFLAGS` (trace.c:216), `NOXSHIFTDIRECT` (x86_64/translate.c:428),
`PROF` (proc.c:444,456,466, linux_aarch64.c:521, linux_x86_64.c:213, x86_64/abi.h:54; forwarded),
`S3DB_DURABILITY` (helpers.c:479), `SHADOWGATE` (aarch64/translate.c:606),
`T2DUMP` (aarch64/translate.c:1967, x86_64/translate.c:3607; forwarded),
`TIER2_SELFTEST` (linux_aarch64.c:454), `TIER2_THRESHOLD` (engine/cache.c:715),
`TIER2X_THRESHOLD` (linux_x86_64.c:221), `VDBETRACE` (linux_aarch64.c:523),
`VTHITCOUNT` (linux_aarch64.c:524), `W4_NOOPENCACHE` (fscache.c:684).

The `pcache` cache-id folds a subset of these codegen toggles into its hash — array in
`translate/aarch64/pcache.c:230` (`NOSTEAL1617, NOGUESTFOLD, NOSHADOWTUNE, SHADOWGATE, NOSTITCH,
NOLSE, NOTIER2, NOIRQSLIM, NOIRQCHECK, NOIBSLIM, CTXDISP`) read via `getenv(envs[i])` at pcache.c:240.

### 1e. Cargo compile-time env (produced by build.rs `rustc-env`, consumed by `env!`) — BRANDED

| Name | Produced | Consumed | Purpose | Target |
|---|---|---|---|---|
| `DDJIT_LINUX_AARCH64` | build.rs:99 (`rustc-env`) | guest.rs:66 | baked path to `ddjit-linux_aarch64` engine | `HL_JIT_LINUX_AARCH64` |
| `DDJIT_LINUX_X86_64` | build.rs:99 | guest.rs:67 | baked path to x86-64 engine | `HL_JIT_LINUX_X86_64` |
| `DDJIT_DARWIN_AARCH64` | build.rs:99 | guest.rs:68 | baked path to darwin engine | `HL_JIT_DARWIN_AARCH64` |
| `DDJAIL_DARWIN_AARCH64` | build.rs:132,142 | guest.rs:76 | baked path to `darwinjail.dylib` | `HL_JAIL_DARWIN_AARCH64` |

Standard cargo/toolchain env in build.rs (KEEP, not dd-branded): `CARGO_MANIFEST_DIR` (build.rs:16),
`OUT_DIR` (build.rs:18), `CC` (build.rs:32), `AR` (build.rs:33), `CARGO_CFG_TARGET_OS` (build.rs:59).

---

## §2. BRANDED SYMBOLS (functions / macros / types / constants)

### 2a. FFI / ABI surface — the load-bearing contract (rename C + Rust IN LOCKSTEP)

Header: `dd-jit-darwin/src/runtime/include/ddjit_api.h`.

| Symbol | Kind | Definition | Mirror / users | Target |
|---|---|---|---|---|
| `ddjit_spawn` | C fn (public ABI) | ddjit_api.h:63 (proto), os/darwin/ffi.c (def) | Rust `extern "C"` decl **dd-jit-darwin/src/launch/spawn.rs:19**, called spawn.rs:71 | `hl_spawn` |
| `ddjit_run_configfd` | C fn (public ABI) | os/ddjit_configfd.c:47 (proto ddjit_api-adjacent) | included into engine TUs; Rust doc wire.rs:2,208 | `hl_run_configfd` |
| `struct ddjit_config` | C struct (wire ABI, 112 B) | ddjit_api.h:22 | Rust `WireHeader` mirror **launch/wire.rs** (fields at wire.rs:9,68,164) | `hl_config` |
| `DDJIT_CONFIG_MAGIC` | C macro `0x44434647` 'DCFG' | ddjit_api.h:19 | Rust const **launch/wire.rs:7** (`0x4443_4647`), used wire.rs:117,190,234 | `HL_CONFIG_MAGIC` |
| `DDJIT_SPAWN_SETPGID` | C macro `0x1` | ddjit_api.h:59 | spawn flags | `HL_SPAWN_SETPGID` |
| `DDJIT_SPAWN_TTY` | C macro `0x2` | ddjit_api.h:60 | spawn flags | `HL_SPAWN_TTY` |
| `DDJIT_API_H` | include guard | ddjit_api.h:12 | — | `HL_API_H` |
| `ddjit_configfd.c` | filename (unity-included) | os/ddjit_configfd.c | included by linux_aarch64.c:89, jitdarwin.c:458 | `hl_configfd.c` |

`dd_run` is the engine entry the configfd bridge dispatches to — see §2b (part of ABI seam because
`ddjit_run_configfd` → `dd_run`).

### 2b. Engine C functions with `dd_` prefix (internal — safe block-rename `dd_` → `hl_`)

Definitions (usage counts from grep across `dd-jit-darwin/`):

| Symbol | Definition | ~uses | Note |
|---|---|---|---|
| `dd_run` | targets/linux_x86_64.c:359, targets/linux_aarch64.c:643, os/darwin/jitdarwin.c:402, decl os/ddjit_configfd.c:20 | 42 | engine main entry (per-target TU) |
| `dd_restore` | targets/linux_aarch64.c:638 | 4 | checkpoint restore entry |
| `dd_unlock` | os/linux/syscall/sysv.c:347 | 73 | SysV IPC spinlock unlock |
| `dd_lock` | sysv.c:327 | 26 | SysV IPC spinlock lock |
| `dd_access` | sysv.c:461 | 20 | IPC perm check |
| `dd_now` | sysv.c:281 | 14 | monotonic time helper |
| `dd_id` | sysv.c:501 | 10 | IPC id alloc |
| `dd_owner` | sysv.c:474 | 8 | IPC owner check |
| `dd_ctrl_name` | sysv.c:314 | — | control-block shm name (`/di%08xC`) |
| `dd_shm_name` | sysv.c:318 | 5 | shm name (`/di%08xs%x`) |
| `dd_msg_name` | sysv.c:322 | 4 | msg-queue name (`/di%08xm%x`) |
| `dd_msg_store` / `dd_msg_uncache` | sysv.c (near 450) | 4 / 2 | msg cache |
| `dd_pground` | sysv.c:285 | 3 | page-round helper |
| `dd_flock` | os/linux/syscall/helpers.c:100 | 3 | flock helper |
| `dd_online_cpus` | os/linux/syscall/dispatch.c:355 | 4 | online CPU count |
| `dd_get_procinfo` / `dd_procinfo` | vfs.c:1710 / struct | 8 | per-pid proc info |
| `dd_gpu_alloc` | vfs.c:3535 | 5 | IOSurface GPU alloc |
| `dd_gpu_send_port` | vfs.c:3464 | 3 | mach port send |
| `dd_gpu_reg_find`/`_add`/`_free_fd`/`dd_gpu_reg_ent` | vfs.c:3502/3508/3523 | 2 each | GPU fd registry |
| `dd_parse_id` / `dd_parse_u64` / `dd_parse_port` / `dd_parse_port_field` | os/container_parse.h:41 (+others) | 13/7/4/3 | config-string parsers |
| `dd_rep_movs` / `dd_rep_stos` | translate/x86_64/translate/repstr.c:31/70 | 4 / 2 | x86 REP string ops |
| `dd_ctrl` | (IPC control block var/type) | 13 | |
| `dd_jit` | (identifier in comments/strings) | 2 | |
| `dd_tmpfile_` | os/linux/syscall/fs.c:1504 (name fragment) | 1 | temp-file name prefix `.dd_tmpfile_%d_%d` |

### 2c. Branded C structs / types (internal → `dd`/`ddjit` → `hl`)

`struct ddjit_config` (ABI, see 2a); `struct ddshm`, `struct ddmsgq`, `struct ddmsg_slot`,
`struct ddmsg_store`, `struct ddsem`, `struct ddlock`, `struct ddperm`, `struct ddipc_ctrl`,
`struct dd_procinfo`, `struct dd_gpu_alloc`, `struct dd_gpu_reg_ent`, `dd_gpu_msg_t` (typedef).
All in `os/linux/syscall/sysv.c` (IPC) and `vfs.c`/`include/dd_gpu.h` (GPU).

### 2d. `DDJIT_*` / `DD_*` C macros & constants NOT tied to an env var (internal → `HL_*`)

- Include guards: `DDJIT_API_H`, `DD_GPU_H` (include/dd_gpu.h:10), `DD_CONTAINER_PARSE_H`
  (os/container_parse.h).
- Build/mode macros: `DDJIT_NO_MAIN` (6 uses — suppresses `main()` when a TU is unity-included),
  `DDJIT_LIB` (2 uses).
- GPU (include/dd_gpu.h): `DD_IOCTL_GPU_ALLOC` (0xC020DD01), `DD_GPU_FMT_BGRA8888`, `DD_GPU_FMT_*`,
  `DD_DMABUF_MOD_MAGIC` (0x6464 = literal ASCII 'dd' — magic **doubly** encodes the brand),
  `DD_DMABUF_RENDER_BIT`, `DD_GPU_REG_MAX`.
- IPC limits (sysv.c:155-164): `DDIPC_SHMMAX`, `DDIPC_SHMMNI_ADV`, `DDIPC_SEMMNI_ADV`,
  `DDIPC_SEMMSL_ADV`, `DDIPC_SEMMNS_ADV`, `DDIPC_SEMOPM_ADV`, `DDIPC_SEMVMX`, `DDIPC_MSGMAX`,
  `DDIPC_MSGMNB`, `DDIPC_MSGMNI_ADV`, plus `DDIPC_SHMMNI`.
- Container limits/consts: `DD_CAP_DEFAULT`, `DD_RLIM_MAX`, `DD_SHMAT_MAX`, `DD_UNDO_MAX`,
  `DD_MSGCACHE_MAX`, `DD_NGROUPS_MAX`, `DD_NOXC_N`, `DD_DNS_NS`, `DD_SI_TIMER`, `DD_SI_MESGQ`,
  `DD_SCHED_RESET_ON_FORK`, `DD_HAS_MACH_EXC`.
- xattr macro constants (VALUES are on-disk keys — see §3): `DD_XATTR_UID` = `"user.dd.uid"`
  (state.c:224), `DD_XATTR_GID` = `"user.dd.gid"` (state.c:225).

> Note: several `DD_*` names above (`DD_CAP_DEFAULT`, `DD_RLIM_MAX`, `DD_XATTR_UID`, …) are C
> macro CONSTANTS, **not** environment variables — do not confuse with §1a. They are internal;
> renaming is cosmetic except `DD_XATTR_UID/GID` whose string VALUES persist on disk.

### 2e. Rust module / type / crate identifiers

- Crate/lib names: `dd_jit` (dd-jit lib), `dd_jit_darwin` (dd-jit-darwin lib) — referenced ~30×
  across `dd-jit/src/**` (`use dd_jit_darwin::…`, e.g. runtime.rs:8,45,120,125; container/mod.rs:5,37,42,44;
  engine/mod.rs:10,80,106; engine/io.rs:2,24,67; error.rs:3; image.rs:4; builder.rs:7; lib.rs:9,26,33;
  examples/run_container.rs:8,10).
- Rust FFI: `extern "C" { fn ddjit_spawn(...) }` (spawn.rs:15-19), `#[link_name="kill"/"waitpid"]`
  (dd-jit/src/runtime/handle.rs:80-83 — NOT branded, keep).
- Test/temp identifiers containing brand: `"ddjit-resolve-user-…"` temp dir name
  (dd-jit/src/runtime/container/user.rs:57).

---

## §3. NAMES: crates, artifacts, paths, on-disk keys, user-facing strings

### 3a. Cargo package / target names

| Item | Location | Value | Target |
|---|---|---|---|
| package | dd-jit/Cargo.toml:2 | `dd-jit` | `husklet-jit` |
| lib name | dd-jit/Cargo.toml:9 | `dd_jit` | `husklet_jit` |
| package | dd-jit-darwin/Cargo.toml:2 | `dd-jit-darwin` | `husklet-jit-darwin` |
| lib name | dd-jit-darwin/Cargo.toml:11 | `dd_jit_darwin` | `husklet_jit_darwin` |
| dep ref | dd-jit/Cargo.toml:12 | `dd-jit-darwin = { path = "../dd-jit-darwin" }` | update path + name |
| descriptions | both Cargo.toml `description=` | prose "dd — …", "dd macOS-host backend" | rebrand prose |

Dependents outside scope that name these crates (flagged for later pass): any `dd-daemon` /
`dd-cli` `Cargo.toml` depending on `dd-jit`.

### 3b. Emitted artifacts / build outputs (baker in build.rs ↔ runtime resolver in guest.rs)

| Artifact | Produced (build.rs) | Resolved (guest.rs) | Target |
|---|---|---|---|
| `ddjit-linux_aarch64` / `ddjit-linux_x86_64` / `ddjit-darwin_aarch64` | build.rs:63 `out.join(format!("ddjit-{t}"))` | guest.rs:64 `format!("ddjit-{}", self.target())` | `hl-<target>` |
| `darwinjail.dylib` | build.rs:113 | guest.rs:76 | keep or `hl-jail.dylib` |
| `libddjit_ffi.a` / `ddjit_ffi.o` | build.rs:31-33; link `static=ddjit_ffi` build.rs:53 | linked into `dd_jit_darwin` | `libhl_ffi.a` |

The engine artifact name is a **string contract**: build.rs bakes `ddjit-<t>` and guest.rs
`resolve_bundled` reconstructs the identical string — rename both or resolution breaks.

### 3c. On-disk paths & keys (MIGRATION-sensitive)

| String | Location | Kind |
|---|---|---|
| `user.dd.uid` / `user.dd.gid` | state.c:224-225 (`DD_XATTR_UID`/`_GID`) | **persisted xattr** on container files (owner emulation) |
| `user.ddx.` (`DDX_PFX`) | os/linux/syscall/fs.c:249 | **persisted xattr** namespace prefix (guest xattrs) |
| `.dd_tmpfile_%d_%d` | fs.c:1504 | temp-file name during rename-emulation |
| `/tmp/dd-lo-%s` | targets/linux_x86_64.c:140 | private-loopback netns socket dir |
| `/tmp/.ddshm-` | os/linux/fscache.c:40 | POSIX shm fallback prefix (no rootfs) |
| `/tmp/ddjit-pcache` / `/tmp/ddjit-pcache-arm64` | x86_64/pcache.c:198, aarch64/pcache.c:269 | default persistent-code-cache dir |
| `/tmp/ddjitd-worker.log` | os/linux/forkserver.c:314,332 | forkserver worker log |
| `/var/lib/dd/alpine` | dd-jit/examples/run_container.rs:11 | example default rootfs |
| `/home/dd/pcache` | dd-jit/src/runtime/container/mod.rs:105, builder.rs:277,292 | example/test pcache dir |
| `com.dd.display.gpu` | os/linux/container/vfs.c:3466 | **mach bootstrap service name** — cross-crate w/ `dd-display` |

Note: SysV IPC shm/sem/msg names use prefix `/di…` (`dd_ctrl_name`/`dd_shm_name`/`dd_msg_name`,
sysv.c:314-323) — that is **not** the `dd` brand ("di" = IPC), safe to leave.
`DD_DMABUF_MOD_MAGIC 0x6464` equals ASCII `"dd"` — an on-wire magic that encodes the brand.

### 3d. User-facing brand strings

- **Default hostname `"jit"`** — used when no `DD_HOSTNAME`: `os/linux/container/vfs.c:2791`
  (`/etc/hostname` read) and `os/linux/syscall/misc.c:16` (uname). Visible inside every container.
  → `"husklet"`?
- **`"dd: …"` error/log prefix** — 27 occurrences across engine TUs. Files & counts:
  container/state.c (6), jail.c (5), os/container_parse.h (5), ddjit_configfd.c (4), os/linux/elf.c (3),
  targets/linux_x86_64.c (1), targets/linux_aarch64.c (1), jitdarwin.c (1), engine/cache.c (1).
  Examples: `"dd: --configfd: bad magic …"` (ddjit_configfd.c:56), `"dd: load_elf: cannot map …"`
  (elf.c:568), `"dd: too many DD_PUBLISH entries"` (state.c:528). → `"husklet: …"`.
- **`"[dd] …"` bracket log prefix** — x86_64 translator/loader diagnostics:
  dispatch_hooks.h:101,214,270,277,291,300; x86_64/elf.c:261; x86_64/translate.c:975,3643. → `"[hl]"`.
- **`"[ddjitd] …"`** forkserver log — forkserver.c:430,464. → `"[hld]"`.
- **`"[ddepollprof] …"`** — helpers.c:942.
- **`ddjit --server/--client/--client:` usage strings** — forkserver.c:360,644,671. → `hl …`.
- Cargo `description` prose in both Cargo.toml files begins "dd — …".
- README.md / docs in both crates contain the brand throughout (not enumerated line-by-line;
  bulk-rebrand as docs).

---

## Risk notes (do these in lockstep / handle carefully)

### Load-bearing ABI/FFI — rename C + Rust in the SAME commit
1. `ddjit_spawn` (C `os/darwin/ffi.c` / `ddjit_api.h:63`) ↔ Rust `extern "C"` **spawn.rs:19**.
   A one-sided rename = link failure.
2. `struct ddjit_config` (C, 112-byte wire layout, `ddjit_api.h:22`) ↔ Rust `WireHeader`
   (**launch/wire.rs**). Field OFFSETS matter; both must change together. The magic
   `DDJIT_CONFIG_MAGIC` is hard-coded in **both** (`ddjit_api.h:19`, `wire.rs:7`) — a mismatch is a
   silent runtime "bad magic" (`ddjit_configfd.c:56`) that refuses every launch.
3. `ddjit_run_configfd` → `dd_run` seam (`os/ddjit_configfd.c`): the configfd bridge translates the
   typed config back into `DD_*`/`DDJIT_*` **setenv** calls (ddjit_configfd.c:76-135) that the target
   TUs then **getenv**. So renaming a `DD_*` env var means editing BOTH the setter (ddjit_configfd.c
   / targets/*.c / proc.c:42 / forkserver.c:330) AND every reader — they are in different files/TUs.

### Cross-crate env vars (set OUTSIDE these two crates, read INSIDE — coordinate with a later pass)
Grep of `dd-cli`/`dd-daemon`/`dd-client`/`dd-gpu`/`dd-display` shows these branded vars set there and
consumed by this engine (so a rename here alone breaks them):
`DD_UID`, `DD_GID`, `DD_VOLUMES`, `DD_NETNS`, `DD_NETBR`, `DD_HOSTNAME`, `DD_GUEST_ENV`, `DD_IP`,
`DD_ULIMITS`, `DD_ROOTFS_RO`, `DD_PUBLISH_DAEMON`, `DD_NET_ISOLATE`, `DD_FSGEN_FILE`, `DD_CPUS`,
`DD_GPU_IOSURFACE`, `DDJIT_RESTORE_DIR`, `DDJIT_DIR`, `DDJIT_CHECKPOINT_DIR`, and the GPU/display
family `DD_GPU_EXEC_SOCK`, `DD_DMABUF_MOD_MAGIC`/`DD_DMABUF_RENDER_BIT`, `DD_DISPLAY_*`, `DD_CUDA_*`,
`DD_PTX` (the last group flows dd-gpu/dd-display ↔ engine GPU path). **These are the true
must-be-atomic env renames.** The `--configfd` typed path is meant to REPLACE the `DD_*` env dialect
at the dd-jit↔engine seam (see ddjit_api.h header comment), so the primary env exposure is: (a) the
GPU/display vars, (b) the still-env-driven darwinjail (`jail.c` reads `DD_*` directly), (c) the
forwarded tuning knobs.

### On-disk / persisted (rename = migration or compat break for existing containers)
- xattr keys `user.dd.uid` / `user.dd.gid` (state.c:224-225) and `user.ddx.*` (fs.c:249) are written
  onto real files in the rootfs/overlay upper. Renaming the namespace orphans owner/xattr metadata on
  already-created containers — needs a migration shim or a read-both/write-new window.
- mach service `com.dd.display.gpu` (vfs.c:3466) must match the name `dd-display` registers.

### Probably NOT worth renaming (internal, invisible to users)
- All §1d bare-control flags (`JT`, `NOSMC`, `PROF`, `T2DUMP`, `NOXAL*`, `NOSTITCH`, `NOLSE`, …):
  no dd branding, pure engine A/B kill-switches. Leave as-is (renaming would only churn the pcache
  cache-id array at aarch64/pcache.c:230 and the mac-bridge forward lists).
- §1c `DDDBG_*` / `DDFAILMMAP` / `DDFAILMPROT` / `DDEPOLLPROF` / `DDRELRODYN`: dev-only debug gates,
  nothing sets them in production. Optional rename.
- The `/di…` IPC name prefix (not brand) and standard cargo env (`OUT_DIR`, `CC`, …): keep.

### High-value user-facing renames (worth doing)
The `DD_*` container-config env (§1a), the default hostname `"jit"`, the `"dd:"`/`"[dd]"` log
prefixes, the crate/lib names, and the emitted artifact names (`ddjit-*`) — these are what a user or
integrator actually sees or links against.

---

## NOT covered by this pass (for the next pass to extend)
- Crates: `dd-cli`, `dd-daemon`, `dd-client`, `dd-gpu`, `dd-display`, `dd-gui`, `dd-images`,
  `dd-tests`, `dd-term-core`, and the workspace-root `Cargo.toml` / `Makefile` / `README.md` /
  `nix/` / `website/` / `assets/`.
- Only their env-var / ABI overlap with `dd-jit`+`dd-jit-darwin` is noted above (§Risk notes),
  not their internal `dd_*` symbols, paths, or strings.
- README.md and `docs/` inside the two in-scope crates were flagged as brand-bearing but not
  enumerated line-by-line (bulk doc rebrand).
