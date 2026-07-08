# dd → husklet rebrand inventory: `dd-cli/` + `dd-daemon/` + `dd-client/` + `dd-images/`

READ-ONLY map, produced 2026-07-07. Step 2 of the `dd`→`husklet` rebrand — the **CLI + daemon +
client + images** layer. This is the *other half* of the lockstep boundary with the engine
(`dd-jit`/`dd-jit-darwin`, mapped in [`dd-jit-inventory.md`](dd-jit-inventory.md)): this layer is
where the container is described and launched, and where a handful of `DD_*`/`DDJIT_*` env vars that
the engine reads are **SET**.

Scope: `dd-cli/`, `dd-daemon/`, `dd-client/`, `dd-images/` only. `dd-gpu`, `dd-display`, `dd-gui`,
`dd-term-core`, `dd-tests`, and the workspace root (`Cargo.toml`/`Makefile`/`README.md`/`nix/`/
`website/`/`assets/`) are OUT of scope — where they set/read a var or name that crosses into these
four crates it is flagged in §Risk notes. **Nothing was renamed. All `file:line` refs are real.**

Decided scheme (from the parent task): env `DD_*`/`DDJIT_*`/bare-flags → `HL_*`; code `dd_*`/`ddjit_*`
→ `hl_*`; crates `dd-X`→`husklet-X` (idents `dd_x`→`husklet_x`); fresh cutover `~/.dd`→`~/.husklet`,
`/tmp/dd-*`/`/tmp/ddjit-*`→`/tmp/hl-*`, `com.dd.*`→`com.husklet.*`; brand strings → `husklet`, short
prefixes → `hl`/`HL`. Binary names FLAGGED for the user (see §3a).

---

## Summary counts

| Category | Count |
|---|---|
| Distinct env-var names touched (read/set/inject) in this layer | **13** branded + `HOME` (keep) |
| — `DD_*` container/daemon-config class | 8 (`DD_IMAGES`, `DD_STATE`, `DD_VOLUMES`, `DD_DEBUG`, `DD_ENGINE_DIR`, `DD_DAEMON_BIN`, `DD_MAC_IMAGE`, `DD_DISPLAY_SOCK`, `DD_GPU_EXEC_SOCK`) — 9 names |
| — `DDJIT_*` engine class (SET here, READ by engine — cross-crate) | 3 (`DDJIT_DIR`, `DDJIT_CHECKPOINT_DIR`, `DDJIT_RESTORE_DIR`) |
| — `DDOCKERD_*` daemon-socket class | 1 (`DDOCKERD_SOCK`) |
| Branded Rust symbols (`dd_*` fns, `DdJit*` types, `ddjit`/`dd_jit` crate refs) | 4 local defs + ~30 crate-ref sites |
| Crates / lib targets / bin targets | 4 pkgs, 3 libs, 2 bins |
| On-disk paths / sockets / launchd+context names / archive keys | ~20 distinct |
| User-facing brand strings (log prefixes, `docker info`/`version`, CLI help) | ~40 sites |

### Rename-mapping proposal (legend — CONFIRM with user before executing)

| From | To (proposed) | Note |
|---|---|---|
| env `DD_IMAGES`/`DD_STATE`/`DD_VOLUMES`/`DD_DEBUG` | `HL_IMAGES`/`HL_STATE`/`HL_VOLUMES`/`HL_DEBUG` | daemon config; SET by dd-cli, READ by daemon (intra-layer lockstep) |
| env `DDOCKERD_SOCK` | `HL_SOCK` (or `HUSKLET_SOCK`) | daemon listen socket; SET by dd-cli, READ by daemon + `dd-client` |
| env `DDJIT_DIR`/`DDJIT_CHECKPOINT_DIR`/`DDJIT_RESTORE_DIR` | `HL_DIR`/`HL_CHECKPOINT_DIR`/`HL_RESTORE_DIR` | **SET here, READ by the engine — CROSS-CRATE lockstep** (see §Risk) |
| env `DD_ENGINE_DIR`/`DD_DAEMON_BIN`/`DD_MAC_IMAGE` | `HL_ENGINE_DIR`/`HL_DAEMON_BIN`/`HL_MAC_IMAGE` | dev/deploy overrides |
| env `DD_DISPLAY_SOCK`/`DD_GPU_EXEC_SOCK` | `HL_DISPLAY_SOCK`/`HL_GPU_EXEC_SOCK` | READ here as host override; **SET by dd-gpu/dd-display — cross-crate w/ a 3rd layer** |
| crates `dd-cli`/`dd-client`/`dd-daemon`/`dd-images` | `husklet-cli`/`husklet-client`/`husklet-daemon`/`husklet-images` | libs `dd_client`/`dd_daemon`/`dd_images` → `husklet_*` |
| bins `ddcli`, `dd-daemon` | **FLAG** (`hl`? `husklet`? / `husklet-daemon`?) | user-typed CLI name — user must choose |
| `dd_root()`/`dd_home()`/`DdJitPty` | `hl_root()`/`hl_home()`/`HlPty` | internal Rust symbols |
| `~/.dd` tree, `/var/lib/dd`, `/tmp/.ddbr-*`, `/tmp/.ddnet-*` | `~/.husklet`, `/var/lib/husklet`, `/tmp/.hlbr-*`, `/tmp/.hlnet-*` | **`.ddbr`/`.ddnet` are CROSS-CRATE w/ engine — lockstep** (see §Risk) |
| launchd `com.dd.daemon`, `/Applications/dd.app` | `com.husklet.daemon`, `/Applications/husklet.app` | user-visible service/app |
| docker context name `"dd"` | `"husklet"` | shown in `docker context ls` |
| `docker info`/`version` brand (`"dd"`, `"dd-jit"`, `"…-dd"`) | `husklet`/`husklet-jit`/`…-husklet` | user-visible via `docker info` |
| default mac image `huttarichard/ddmac:latest` | user to decide (registry rename) | external artifact |
| archive keys `dd-manifest.json`/`dd-image.json` | `hl-manifest.json`/`hl-image.json`? | **persisted inside tar archives — save/load compat** (see §Risk) |

---

## §1. ENVIRONMENT VARIABLES (the priority)

Read = `std::env::var`/`var_os`. Set = `std::env::set_var`. Inject = `Command::.env(...)`.
**Key architectural fact:** the daemon does **NOT** set `DD_*` container-config vars directly. It
drives the engine through the **typed `ddjit::Container`/`Image` builder** (`dd-daemon/src/runtime/
spawn/mod.rs`); dd-jit's `--configfd`/`setenv` path turns those typed calls into the engine's
`DD_UID`/`DD_HOSTNAME`/… (see the dd-jit inventory §Risk). So the cross-crate env SETTERS in *this*
layer are only the three `DDJIT_*` vars set by **dd-cli** (§1b), plus the typed-builder method map
(§1d). The `DD_*` names in §1a are daemon-internal config (dd-cli sets → daemon reads), NOT engine
reads.

### 1a. `DD_*` + `DDOCKERD_SOCK` — daemon/CLI config class (intra-layer: dd-cli SETS → daemon/client READ)

| Name | Read sites | Set / inject sites | Purpose | Target |
|---|---|---|---|---|
| `DDOCKERD_SOCK` | dd-daemon/src/main.rs:66; dd-client/src/lib.rs:52 | dd-cli/src/wsdaemon.rs:45 (`.env`), dd-cli/src/daemon.rs:38 (`.env`) | daemon listen/connect socket path (default `./dd.sock` / `~/.dd/run/docker.sock`) | `HL_SOCK` |
| `DD_IMAGES` | dd-daemon/src/main.rs:65 | dd-cli/src/wsdaemon.rs:46, dd-cli/src/daemon.rs:39 | image rootfs store dir (default `./images`) | `HL_IMAGES` |
| `DD_STATE` | dd-daemon/src/main.rs:67 | dd-cli/src/wsdaemon.rs:47 | container/vol/net state json (default `~/.dd/state.json`) | `HL_STATE` |
| `DD_VOLUMES` | dd-daemon/src/main.rs:69 | dd-cli/src/wsdaemon.rs:48 | named-volumes dir | `HL_VOLUMES` — ⚠ collides w/ engine's darwinjail `DD_VOLUMES` (dd-jit jail.c:232), a DIFFERENT var, same name → same `HL_VOLUMES` (see §Risk) |
| `DD_DEBUG` | dd-daemon/src/main.rs:145, runtime/spawn/live.rs:88,107,119,201, containers/ports.rs:133, containers/lifecycle/run.rs:37,214 | — (set by operator env) | verbose daemon logging gate | `HL_DEBUG` |
| `DD_ENGINE_DIR` | dd-cli/src/wsdaemon.rs:54 | — (operator env) | where `ddjit-*` engines live (fallback: next to daemon) | `HL_ENGINE_DIR` |
| `DD_DAEMON_BIN` | dd-cli/src/wsdaemon.rs:83, dd-cli/src/paths.rs:55 | — (operator env) | override path to the `dd-daemon` binary | `HL_DAEMON_BIN` |
| `DD_MAC_IMAGE` | dd-cli/src/run.rs:62 | — (operator env) | override default macOS-container image | `HL_MAC_IMAGE` |

### 1b. `DDJIT_*` — engine class **SET in this layer, READ by the engine (CROSS-CRATE lockstep)**

These are the true cross-crate env SETTERS this layer owns. Renaming them requires editing the engine
readers in the SAME commit (reader sites cited from the dd-jit inventory).

| Name | Set sites (this layer) | Engine read sites (dd-jit) | Purpose | Target |
|---|---|---|---|---|
| `DDJIT_DIR` | dd-cli/src/wsdaemon.rs:56 (`.env`), dd-cli/src/daemon.rs:41 (`.env`) | dd-jit-darwin/src/guest.rs:90 | dir to locate `ddjit-*` engine artifacts | `HL_DIR` |
| `DDJIT_CHECKPOINT_DIR` | dd-cli/src/ddjit_launcher.rs:79 (`set_var`) | os/linux/checkpoint.c:178 | CRIU-style checkpoint dir (armed for every workspace launch) | `HL_CHECKPOINT_DIR` |
| `DDJIT_RESTORE_DIR` | dd-cli/src/ddjit_launcher.rs:81 (`set_var`), removed :83 | os/linux/checkpoint.c:177, targets/linux_aarch64.c:646,820 | checkpoint restore dir (set only when resuming) | `HL_RESTORE_DIR` |

### 1c. `DD_DISPLAY_SOCK` / `DD_GPU_EXEC_SOCK` — GPU/display class (READ here, SET by dd-gpu/dd-display)

Host-side socket-path **overrides** read here to build the `dd_gpu` DeviceProvider. They are part of
the broader `DD_GPU_*`/`DD_DISPLAY_*`/`DD_CUDA_*` contract that flows between dd-gpu/dd-display and
the engine (see dd-jit inventory §Risk). Renaming crosses THREE layers.

| Name | Read sites | Purpose | Target |
|---|---|---|---|
| `DD_DISPLAY_SOCK` | dd-cli/src/ddjit_launcher.rs:239 | host wayland socket override (else `~/.dd/run/wayland-0`) | `HL_DISPLAY_SOCK` |
| `DD_GPU_EXEC_SOCK` | dd-cli/src/ddjit_launcher.rs:241 | host GPU-exec socket override (else `<dir>/dd-gpu.sock`) | `HL_GPU_EXEC_SOCK` |

### 1d. Typed-builder → engine `DD_*` map (NOT env in this layer, but the real cross-crate contract)

`dd-daemon/src/runtime/spawn/mod.rs:42-147` and `dd-cli/src/ddjit_launcher.rs:146-210` describe the
container via typed `ddjit`/`dd_jit` builder calls; dd-jit encodes each into the engine env dialect.
These are NOT env sites here (no rename needed in these files for the env name) but are listed so the
lockstep is visible — the ENGINE-side reader of each renames per the dd-jit inventory:

| Builder call (spawn/mod.rs) | Engine env it becomes | 
|---|---|
| `.guest_env(&c.env, c.tty)` :48 | `DD_GUEST_ENV` → `HL_GUEST_ENV` |
| `.hostname(...)` :50 | `DD_HOSTNAME` → `HL_HOSTNAME` |
| `.memory_bytes(...)` :51 | `DD_MEM_MAX` → `HL_MEM_MAX` |
| `.pids(...)` :52 | `DD_PIDS_MAX` → `HL_PIDS_MAX` |
| `.cpus(...)` :54 | `DD_CPUS` → `HL_CPUS` |
| `.read_only(...)` :55 | `DD_ROOTFS_RO` → `HL_ROOTFS_RO` |
| `.ulimit(...)` :57 | `DD_ULIMITS` → `HL_ULIMITS` |
| `.private_network(...)` :66 | `DD_NETNS`/`DD_NONETNS` → `HL_NETNS` |
| `.write_coherence_file(...)` :73 | `DD_FSGEN_FILE` → `HL_FSGEN_FILE` |
| `.net_isolate(...)` :76 | `DD_NET_ISOLATE` → `HL_NET_ISOLATE` |
| `.bridge(netid, ip)` :79 | `DD_NETBR`/`DD_IP` → `HL_NETBR`/`HL_IP` |
| `.user_spec(...)` :84 | `DD_UID`/`DD_GID` → `HL_UID`/`HL_GID` |
| `.sandbox(...)` :87 | `DDJIT_SANDBOX`/`DDJIT_UNTRUSTED` → `HL_SANDBOX`/`HL_UNTRUSTED` |
| `.publish(...)` :125 | `DD_PUBLISH` → `HL_PUBLISH` |
| `.external_port_forwarder(...)` :127 | `DD_PUBLISH_DAEMON` → `HL_PUBLISH_DAEMON` |
| `.env(k,v)` (darwin guest env, incl. `PATH`) :139,143 | real guest env — NOT branded, KEEP |

> `HOME` is read at dd-cli/src/paths.rs:15, dd-daemon/src/util/paths.rs:6, dd-client/src/lib.rs:55 —
> standard, NOT branded, KEEP.

---

## §2. BRANDED SYMBOLS (Rust functions / types / crate identifiers)

### 2a. Local branded symbols defined in this layer (internal → `dd_`/`DdJit` → `hl_`/`Hl`)

| Symbol | Kind | Definition | Usage | Target |
|---|---|---|---|---|
| `dd_root()` | fn | dd-cli/src/paths.rs:21 | ddjit_launcher.rs:69,70,72,240,245,251,252,272, workspace.rs, install.rs | `hl_root()` |
| `dd_home()` | fn | dd-daemon/src/util/paths.rs:5 | util/fsgen.rs:19, util/paths.rs:15, system.rs:64, build/prune.rs, +others | `hl_home()` |
| `DdJitPty` | struct + impls | dd-cli/src/ddjit_launcher.rs:383 (impls :395 `PtyBackend`, :452 `Drop`); referenced :353, doc :6, workspace.rs:220 | (adapts `RunningContainer` to `PtyBackend`) | `HlPty`? (drop the `Jit` infix — user to confirm) |
| `ddjit_launcher` | Rust module | dd-cli/src/main.rs:21 (`mod ddjit_launcher;`), file `dd-cli/src/ddjit_launcher.rs` | launch path | `hl_launcher`? / rename file |

`dd_root()` (dd-cli) and `dd_home()` (dd-daemon) are the SAME concept (the `~/.dd` root) named
differently in two crates — NOT a flat-scheme collision (distinct idents), but both must move to
`~/.husklet` together (§3c).

### 2b. Cross-crate crate references — the `dd-jit` engine dependency

The engine crate is `dd-jit` (package). Two different import spellings reach it:

| Crate | Cargo dep line | Import ident | Sites |
|---|---|---|---|
| dd-cli | `dd-jit = { path = "../dd-jit" }` (Cargo.toml:18) | `dd_jit::` | ddjit_launcher.rs:11,22,24,25,26,110,146,178,346, workspace.rs:169 (~10) |
| dd-daemon | `ddjit = { path = "../dd-jit", package = "dd-jit" }` (Cargo.toml:19) | `ddjit::` | main.rs:17,114, build/mod.rs:21, build/steps.rs:140,141,150, images/pull/arch.rs:4, images/pull/config.rs:5, model/mod.rs:2, prelude.rs:4, runtime/mod.rs:6, runtime/spawn/mod.rs:16,59,86, util/mod.rs:11, +doc comments (~20) |
| dd-images | (no dep) doc-only `dd_jit::Error` ref | error.rs:2 comment | 1 |

**Rename ripple:** renaming the `dd-jit` *package* to `husklet-jit` forces:
(1) dd-cli Cargo.toml:18 dep name + all `dd_jit::` → `husklet_jit::`;
(2) dd-daemon Cargo.toml:19 `package = "dd-jit"` → `package = "husklet-jit"` (the local alias `ddjit`
could stay, or become `hl`/`husklet` — user choice; if kept, the `use ddjit::` sites don't change).
Also dd-cli deps on `dd-client`, `dd-term-core`, `dd-images`, `dd-gpu` (Cargo.toml:13,14,19,23) and
dd-daemon dep on `dd-images` (Cargo.toml:20) rename with those crates.

---

## §3. NAMES: crates, bins, paths, sockets, service names, brand strings

### 3a. Cargo package / lib / bin targets

| Item | Location | Value | Target |
|---|---|---|---|
| package | dd-cli/Cargo.toml:2 | `dd-cli` | `husklet-cli` |
| **bin name** | dd-cli/Cargo.toml:9 | `ddcli` | **FLAG** — `hl`? `husklet`? (the user-typed command) |
| package | dd-client/Cargo.toml:2 | `dd-client` | `husklet-client` |
| lib name | dd-client/Cargo.toml:9 | `dd_client` | `husklet_client` |
| package | dd-daemon/Cargo.toml:2 | `dd-daemon` | `husklet-daemon` |
| lib name | dd-daemon/Cargo.toml:11 | `dd_daemon` | `husklet_daemon` |
| **bin name** | dd-daemon/Cargo.toml:15 | `dd-daemon` | **FLAG** — `husklet-daemon`? (also the on-disk filename resolved at paths.rs:58,64) |
| package | dd-images/Cargo.toml:2 | `dd-images` | `husklet-images` |
| lib name | dd-images/Cargo.toml:9 | `dd_images` | `husklet_images` |
| descriptions | all four `Cargo.toml` `description=` | prose mentioning "dd", "ddcli", "dd-daemon", "dd-jit" | rebrand prose |

### 3b. On-disk paths, sockets, launchd + docker-context names

| String | Location | Kind | Target |
|---|---|---|---|
| `~/.dd` state root | dd-cli/src/paths.rs:22 (`dd_root`), dd-daemon/src/util/paths.rs:9 (`dd_home`) | state dir | `~/.husklet` |
| `~/.dd/run/docker.sock` | dd-cli/src/paths.rs:31-33; dd-client/src/lib.rs:56; context.rs:3 | daemon socket (`docker.sock` name is docker-compat, keep basename; `.dd` dir renames) | `~/.husklet/run/docker.sock` |
| `./dd.sock` (default) | dd-daemon/src/main.rs:66 | fallback socket | `./husklet.sock`? |
| `~/.dd/run`, `~/.dd/images`, `~/.dd/ws/<name>`, `~/.dd/pcache`, `~/.dd/buildcache`, `~/.dd/nvml`, `~/.dd/bin`, `~/.dd/gui`, `~/.dd/containers/<cid>/fsgen` | paths.rs:26,36; wsdaemon.rs:10; util/paths.rs:15; util/fsgen.rs:19; ddjit_launcher.rs:251,252,264,265,272 | subdirs under state root | move with `~/.husklet` |
| `~/Library/Logs/dd` | dd-cli/src/paths.rs:42 | daemon logs | `~/Library/Logs/husklet` |
| `/var/lib/dd/images`, `/var/lib/dd/volumes` | dd-images/src/image/store.rs:61,66, mod.rs:6; dd-daemon/src/containers/inspect/mounts.rs:121,127 | example/test store roots | `/var/lib/husklet/...` |
| `.dd-write-probe` | dd-daemon/src/containers/lifecycle/create/mod.rs:116 | write-probe filename | `.husklet-write-probe`? (internal) |
| **`/tmp/.ddbr-<netid[..40]>`** | dd-daemon/src/containers/ports.rs:72 (+tests :278,318); comments net.rs:4,11, ports.rs:21 | **AF_UNIX virtual-switch dir — CROSS-CRATE: engine reads at netns.c:636** | `/tmp/.hlbr-*` (lockstep) |
| **`/tmp/.ddnet-<key[..40]>`** | dd-daemon/src/containers/ports.rs:79 (+tests :279,291,318,331,333) | **loopback publish dir — CROSS-CRATE: engine reads at linux_aarch64.c:373** | `/tmp/.hlnet-*` (lockstep) |
| `<g_netbr>/.names` table | dd-daemon/src/runtime/spawn/net.rs:4 (comment) — written by daemon, read by engine netns.c:2236 | endpoint-name table (path derived from `.ddbr` dir) | follows `.ddbr` rename |
| `com.dd.daemon` (`AGENT_LABEL`) | dd-cli/src/paths.rs:8; agent.rs:1,21,103,108,113; comments | launchd label + `gui/<uid>/com.dd.daemon` service | `com.husklet.daemon` |
| `com.dd.daemon.plist` | dd-cli/src/paths.rs:45,49 (`{AGENT_LABEL}.plist`) | plist filename | follows label |
| `/Applications/dd.app` (`APP_BUNDLE`) | dd-cli/src/paths.rs:11; :58 `.../Resources/dd-daemon` | installed bundle | `/Applications/husklet.app` |
| docker context name `"dd"` (`NAME`) | dd-cli/src/context.rs:14 (used :32,41,46,52,57,60,63,72,76,116) | `docker context ls` entry | `"husklet"` |
| default mac image `huttarichard/ddmac:latest` | dd-cli/src/run.rs:73 (`DEFAULT_MAC_IMAGE`) | external registry ref | **FLAG** — external artifact, user to decide |

### 3c. Archive-format keys (dd-images — PERSISTED inside tar archives → save/load compat)

| String | Location | Kind | Target |
|---|---|---|---|
| `dd-manifest.json` | dd-images/src/image/archive/save.rs:21,30; archive/load.rs:47 | **written into `docker save` tar, read back by load** — format contract | `hl-manifest.json`? (breaks cross-version load) |
| `dd-image.json` | dd-images/src/image/archive/mod.rs:97; archive/import.rs:34; image/discovery/env.rs:87; image/discovery/mod.rs:63 | **per-image metadata sidecar written to store + tar** | `hl-image.json`? (persisted) |

### 3d. Ephemeral temp-file prefixes (dd-images — internal, low-risk)

`dd-load-` (archive/load.rs:13), `dd-import-` (import.rs:17), `dd-save-` (save.rs:17), `dd-images-test-`
(archive/mod.rs:113), `dd-reg-`/`dd-reg-body-` (registry/http/curl.rs:24, verbs.rs:66), `dd-layer-`
(registry/client/pull.rs:99), `dd-digest-` (image/digest.rs:99), `dd-wh-test-`/`dd-opq-test-`
(registry/mod.rs:77,103), `dd_layer_wh_test_` (registry/layer.rs:157), `dd_paths_test_`
(dd-daemon/src/util/paths.rs:69). All under `std::env::temp_dir()` → `hl-*`/`hl_*` (cosmetic).

### 3e. User-facing brand strings

**`docker info` / `docker version` responses (dd-daemon/src/system.rs)** — visible to any `docker`
client hitting the daemon:

| Field | Location | Value | Target |
|---|---|---|---|
| `kernel_version` | system.rs:14,62 | `6.1.0-dd` | `6.1.0-husklet`? |
| `git_commit` | system.rs:15 | `dd00000` | rebrand |
| `platform.name` | system.rs:19 | `dd` | `husklet` |
| Engine component `version` | system.rs:22, `server_version` :63 | `0.1.0-dd` | `0.1.0-husklet` |
| info `id` | system.rs:47 | `DD` | `HUSKLET`? |
| info `name` | system.rs:48 | `dd` | `husklet` |
| `operating_system` | system.rs:57 | `dd (VM-less JIT on macOS)` | `husklet (…)` |
| `default_runtime` | system.rs:66 | `dd-jit` | `husklet-jit` |

**Log prefixes:**
- `[dd]` — dd-cli/src/ddjit_launcher.rs:85,131,195,278,294; workspace.rs:203 (6). → `[husklet]`/`[hl]`.
- `[dd-daemon]` — dd-daemon/src/main.rs:94,105,112,132 (6 occurrences). → `[husklet-daemon]`.
- `dd:` error prefix — dd-daemon/src/runtime/spawn/live.rs:143,173 (2). → `husklet:`.

**CLI help / doc brand (dd-cli):**
- `#[command(name = "ddcli", ... about = "ddcli — VM-less containers on macOS")]` — cli.rs:6.
- Module docs + usage examples naming `ddcli`/`dd`: main.rs:1,6-11; cli.rs:1,16,53; install.rs:1,9;
  and the ~30 `"ddcli"` argv strings in cli.rs tests (:162-245) — those are test fixtures, rename with
  the bin name.
- daemon module doc `dd-daemon — … the **dd** VM-less JIT runtime` — main.rs:1,4,6,8-11.
- `dd-client` description prose (Cargo.toml:5) "…dd-daemon…dd CLI and GUI".

---

## §4. Risk / lockstep notes

### A. Cross-crate env SETTERS this layer owns (rename in lockstep with the ENGINE's readers)
The parent task asked to emphasize the vars this layer SETS that the engine consumes. In this layer
that set is **small and precise** — the daemon uses the *typed* `ddjit` builder, not raw `DD_*` env,
so the only direct env SETTERS crossing into the engine are the three **`DDJIT_*`** vars set by
**dd-cli**:
1. `DDJIT_DIR` — dd-cli/src/wsdaemon.rs:56 + daemon.rs:41 → engine guest.rs:90 (`HL_DIR`).
2. `DDJIT_CHECKPOINT_DIR` — dd-cli/src/ddjit_launcher.rs:79 → engine checkpoint.c:178 (`HL_CHECKPOINT_DIR`).
3. `DDJIT_RESTORE_DIR` — dd-cli/src/ddjit_launcher.rs:81 → engine checkpoint.c:177 + linux_aarch64.c:646,820 (`HL_RESTORE_DIR`).

The larger `DD_UID/GID/HOSTNAME/GUEST_ENV/NETNS/NETBR/IP/FSGEN_FILE/CPUS/MEM_MAX/PIDS_MAX/PUBLISH/
PUBLISH_DAEMON/NET_ISOLATE/ROOTFS_RO/ULIMITS` contract flows through the **typed builder** (§1d) — no
env-name literal lives in these four crates for those, so renaming them touches only dd-jit's setter/
reader, NOT this layer's source (except the human-readable comments that name them, e.g. spawn/mod.rs
:128 `DD_GUEST_ENV`, live.rs:22 `DD_NETBR`). Renaming the ENGINE var without touching these comments
is safe at build time but leaves stale doc references — grep-fix the comments too.

### B. Cross-crate ON-DISK path contracts (rename in lockstep with the engine)
- **`/tmp/.ddbr-<netid>`** (daemon: ports.rs:72; engine: netns.c:636) and **`/tmp/.ddnet-<key>`**
  (daemon: ports.rs:79; engine: linux_aarch64.c:373,806, vfs.c:2222) — the AF_UNIX virtual-switch and
  loopback-publish rendezvous dirs. Both the daemon (writer/creator) and the engine (listener) hardcode
  the `/tmp/.ddbr-`/`/tmp/.ddnet-` prefix independently. A one-sided rename silently breaks port
  publishing + container-to-container networking (no error, just no connectivity). The `<g_netbr>/.names`
  endpoint table (net.rs:4 ↔ netns.c:2236) rides on the `.ddbr` dir.
- **`fsgen` file** — the daemon creates `~/.dd/containers/<cid>/fsgen` (util/fsgen.rs:19) and hands its
  PATH to the engine via `.write_coherence_file()` (→ `DD_FSGEN_FILE`). The path travels through the
  typed builder, so only the `~/.dd` → `~/.husklet` dir rename matters here (the `fsgen` basename is not
  branded).

### C. Cross-crate with a THIRD layer (dd-gpu / dd-display)
`DD_DISPLAY_SOCK` / `DD_GPU_EXEC_SOCK` (read at ddjit_launcher.rs:239,241) are host-side overrides SET
by dd-gpu/dd-display (out of scope). The `dd-gpu.sock`/`wayland-0` socket basenames (ddjit_launcher.rs
:240,244,245) and the `~/.dd/gui/<arch>/{lib,bin}`, `~/.dd/nvml/<arch>`, `~/.dd/bin/nvidia-smi*` drop-in
layout (:251-287) are a contract with dd-gpu's provider — coordinate the rename across all three.

### D. Intra-layer env contract (dd-cli SETS → daemon/client READ) — rename together
`DDOCKERD_SOCK`, `DD_IMAGES`, `DD_STATE`, `DD_VOLUMES` are set by dd-cli (wsdaemon.rs:45-48, daemon.rs
:38-39) and read by dd-daemon (main.rs:65-69) and dd-client (lib.rs:52). All within these four crates,
so atomic within this pass — but the daemon and client are **separate binaries/consumers**, so a
half-rename (CLI sets `HL_SOCK`, daemon still reads `DDOCKERD_SOCK`) silently falls back to defaults.

### E. Flat-scheme COLLISIONS / name clashes found
1. **`DD_VOLUMES` is overloaded across the boundary.** In this layer it is the *daemon's named-volumes
   directory* (dd-daemon/src/main.rs:69, set dd-cli wsdaemon.rs:48). In the engine it is the
   *darwinjail per-container volume list* (dd-jit jail.c:232, per the dd-jit inventory §1a). Two
   semantically distinct vars share the name `DD_VOLUMES` today and both naïvely map to `HL_VOLUMES`.
   They never coexist in one process (daemon vs. engine), so a shared `HL_VOLUMES` *preserves* the
   existing (pre-existing) ambiguity — but the user should be aware they are NOT the same variable and
   may want to disambiguate (e.g. daemon `HL_VOLUMES_DIR`).
2. **`DD_SANDBOX` (engine darwinjail) vs `DDJIT_SANDBOX` (engine sentry)** both → `HL_SANDBOX` — noted
   in the dd-jit inventory; this layer only reaches them via `.sandbox()` (§1d), so no new literal here.
3. `dd_root()` (dd-cli) vs `dd_home()` (dd-daemon) — same `~/.dd` concept, distinct idents; NOT a
   symbol collision, but rename both to point at `~/.husklet` in lockstep.

### F. Persisted / compat-sensitive (fresh cutover per scheme, but flag the break)
- Archive keys `dd-manifest.json` / `dd-image.json` (§3c) are written INTO `docker save` tarballs and
  the image store, then read back by load/import/discovery. Renaming them means archives produced by
  the old `dd` cannot be loaded by `husklet` and vice-versa. The scheme says "fresh cutover (no
  back-compat)" — acceptable, but this is the one place an EXTERNAL artifact (a saved tar a user
  already has) breaks. Consider read-both/write-new for load only.
- `com.dd.daemon` launchd label + `/Applications/dd.app`: existing installs have the old label loaded;
  `ddcli uninstall` (old) then `husklet install` (new) is the clean migration — note in release docs.
- `huttarichard/ddmac:latest` default mac image (run.rs:73) is an external registry ref — renaming the
  const is trivial but the image itself must be re-published under any new name.

### G. Probably-cosmetic (internal, low-risk) — rename for consistency, no contract
`dd_root`/`dd_home`/`DdJitPty`/`ddjit_launcher` module (§2a); the dd-images temp prefixes (§3d);
`.dd-write-probe`; `dd_paths_test_`/`dd_layer_wh_test_` test fixtures. `DD_DEBUG` is operator-set at
runtime only (no code sets it) — low value but harmless to rename to `HL_DEBUG`.

---

## NOT covered by this pass
- `dd-gpu`, `dd-display`, `dd-gui`, `dd-term-core`, `dd-tests`, and the workspace root
  (`Cargo.toml`/`Makefile`/`README.md`/`nix/`/`website/`/`assets/`). Their env/name overlap with
  these four crates is flagged in §4C (GPU/display) and §2b (crate-dep ripple) only.
- README.md / doc comments inside the four crates are flagged as brand-bearing (e.g. every module
  header) but not enumerated line-by-line — bulk doc rebrand.
- The engine-side `DD_*`/`DDJIT_*` readers are in the dd-jit inventory, not re-listed here.
