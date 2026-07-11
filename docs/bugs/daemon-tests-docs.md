# Daemon, Test, and Documentation Gaps

This file covers daemon architecture, Docker API mismatches, build/test false greens, and stale documentation.

## Workspace VPN Egress Is Dropped

Priority: P1
Impact: privacy/security expectation violation; traffic can go direct
Confidence: High

Verification status: Proven with failing guard tests in isolated worktree `/Users/x/dd/dd-agent4`.

Verification status: Proven in isolated worktree `/Users/x/dd/verify3-dd-worktree`.

Evidence:

- CLI configures egress through the builder: `dd-cli/src/ddjit_launcher.rs:210`.
- Builder stores `DD_EGRESS_SOCKS`: `dd-jit/src/runtime/container/builder.rs:168`.
- `Container::launch_config` maps selected env keys to typed launch config and drops unknown keys; `DD_EGRESS_SOCKS` is not mapped: `dd-jit/src/runtime/container/mod.rs:63`.
- Engine only enables egress redirect from `getenv("DD_EGRESS_SOCKS")`: `dd-jit-darwin/src/runtime/os/linux/container/netns.c:1616`.

Why this is bad:

A workspace can appear to have VPN/SOCKS egress configured while the typed launch path silently drops the env key. Guest external TCP traffic can go direct.

Verification:

Run a workspace with a SOCKS endpoint pointing to a logging proxy, attempt external TCP from the guest, and confirm whether the proxy receives the connection.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/verify3-target cargo test -p dd-jit --lib launch_config_drops_egress_socks_even_when_builder_sets_it -- --nocapture
```

Result: `1 passed; 0 failed`. The test demonstrates that the builder records `DD_EGRESS_SOCKS` but `launch_config` drops it before engine launch.

Coverage gap:

Add a unit/integration check that every builder env key is either mapped to `LaunchConfig` or explicitly rejected.

## Live Network Connect/Disconnect Mutates Daemon State Only

Priority: P2
Impact: Docker network API reports success before engine observes change
Confidence: High

Verification status: Proven at handler/state level in isolated worktree `/Users/x/dd/verify3-dd-worktree`.

Evidence:

- Running container receives network bridge/IP at spawn time through env/config: `dd-daemon/src/runtime/spawn/mod.rs:78`.
- `network_connect` calls `join_network`, saves state, and returns OK: `dd-daemon/src/networks/handlers.rs:131`.
- `network_disconnect` calls `leave_network`, saves state, and returns OK: `dd-daemon/src/networks/handlers.rs:155`.
- Live DNS `.names` files are generated during spawn: `dd-daemon/src/runtime/spawn/live.rs:62`.

Why this is bad:

`docker network connect` on a running container can report success while the running JIT remains on its original network until restart. Disconnect can leave stale names visible to live peers.

Verification:

Start two containers, connect one to a user network after start, test peer DNS/TCP before and after restarting the connected container.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/verify3-target cargo test -p dd-daemon --bin dd-daemon live_network_connect_disconnect_only_mutates_network_state -- --nocapture
```

Result: `1 passed; 0 failed`. The test verifies endpoint membership changes while the running `Live` object is unchanged.

## Fractional `--cpus` Loses Quota Precision

Priority: P1
Impact: cgroup CPU quota is too high for fractional limits
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-e`.

Evidence:

- `nano_cpus_to_cpus` rounds `NanoCpus` up to whole CPUs: `dd-daemon/src/runtime/spawn/spec.rs:21`.
- Spawn config forwards only the rounded integer CPU count: `dd-daemon/src/runtime/spawn/mod.rs:53`.
- cgroup `cpu.max` renders `g_cpu_max * 100000`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3427`.

Why this is bad:

Docker `--cpus=0.5` should expose quota `50000 100000`. dd rounds it to one CPU, so runtimes sizing from cgroups see twice the requested CPU budget.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-slot-e-target cargo test -p dd-daemon fractional_nano_cpus_needs_fractional_cgroup_quota -- --nocapture
```

Result: failed as intended; left `1`, right `0`.

## Gap and Architecture Docs Are Not Auditable

Priority: P2
Impact: xfail rationale and architecture state drift
Confidence: High

Evidence:

- Source comments reference `docs/GAPS.md`, `docs/SYSCALLS.md`, `docs/IMAGE-MANIFEST.md`, `docs/TESTING.md`, and `docs/CHARTER.md`.
- These files are absent in the current `docs/` root (`test -e` returned nonzero for each during this audit).
- `docs/ENGINE_HOLES.md` says default NaN sign and runtime DF are fixed near the top, but later still lists DIVSS/DIVPS NaN sign and DF as open: `docs/ENGINE_HOLES.md:6`, `docs/ENGINE_HOLES.md:410`, `docs/ENGINE_HOLES.md:414`.
- Current translator code has `emit_dnan_pre/post` and runtime `cpu->df` handling: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:872`, `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:385`.

Why this is bad:

Fix agents and reviewers cannot reliably tell which gaps are accepted, fixed, stale, or still open. Xfail comments name a missing taxonomy, and current architecture docs contradict source state.

Suggested improvement:

Create one canonical gap registry with:

- stable id
- owner
- affected engines
- severity
- source evidence
- test or xfail case
- expected Linux/Docker behavior
- current dd behavior
- close condition

