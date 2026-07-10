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

## Published Port Bind Failures Do Not Fail Start

Priority: P1
Impact: Docker API lies about running/reachable service
Confidence: Medium-high

Verification status: Proven in isolated worktree `/Users/x/dd/verify3-dd-worktree`.

Evidence:

- `containers_start` marks status running before spawn: `dd-daemon/src/containers/lifecycle/run.rs:28`.
- Port forwarders start before JIT launch: `dd-daemon/src/runtime/spawn/live.rs:132`.
- Listener bind failure logs only under `DD_DEBUG` and returns: `dd-daemon/src/containers/ports.rs:128`.
- Start still returns `204`: `dd-daemon/src/containers/lifecycle/run.rs:48`.

Why this is bad:

If an explicit host port is already occupied, Docker-compatible behavior should fail start with a port allocation error. Current flow can report the container running while no dd listener owns the published port.

Verification:

Bind a host port with `nc -l`, then start a container publishing the same port. Inspect start result and actual listener ownership.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/verify3-target cargo test -p dd-daemon --bin dd-daemon start_records_forwarder_even_when_host_bind_fails -- --nocapture
```

Result: `1 passed; 0 failed`. The test demonstrates state records a forwarder/start path even when the host bind fails in the acceptor.

## Inline Volume Sources Can Escape `volumes_dir`

Priority: P1
Impact: host path escape through named-volume-looking input
Confidence: Medium

Verification status: Proven as a compatibility/data-placement bug in isolated worktree `/Users/x/dd/verify3-dd-worktree`.

Evidence:

- Explicit volume creation validates names: `dd-daemon/src/volumes.rs:71`.
- Create persists raw `HostConfig.Binds` / `Mounts`: `dd-daemon/src/containers/lifecycle/create/mod.rs:136`, `dd-daemon/src/containers/lifecycle/create/mod.rs:259`, `dd-daemon/src/containers/lifecycle/create/mod.rs:312`.
- Non-absolute mount sources are resolved with `PathBuf::from(volumes_dir).join(src)`: `dd-daemon/src/runtime/spawn/spec.rs:8`.

Why this is bad:

An inline source like `../../some-host-dir:/mnt` can be treated as a named volume but path-join outside `volumes_dir`. Explicit volume creation rejects bad names, but inline bind/mount input appears to bypass that validation.

Verification:

Use raw Docker API input with `Binds: ["../../x:/mnt"]` or `Mounts: [{Type:"volume", Source:"../../x", Target:"/mnt"}]` and inspect the resolved host path.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/verify3-target cargo test -p dd-daemon --bin dd-daemon resolve_mount_src_dotdot_source_resolves_outside_volumes_dir -- --nocapture
```

Result: `1 passed; 0 failed`. The proof uses `../escaped` and shows the resolved path canonicalizes outside the local volume root.

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

## `docker commit` Drops Container Writes

Priority: P1
Impact: silent data loss in committed images
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-3b-worktree`.

Evidence:

- Commit snapshots `c.rootfs`, the lower immutable image rootfs: `dd-daemon/src/build/prune.rs:102`.
- The snapshot copies that rootfs into the committed image: `dd-daemon/src/build/prune.rs:160`.

Why this is bad:

`docker commit` should capture the container filesystem state, including writable-layer changes. Current behavior can build a new image that silently contains old lower-layer content while reporting commit success.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-target-3b cargo test -p dd-daemon deeper_3b -- --ignored --nocapture
```

PoC `docker commit` case expected `upper\n` from the committed image but read `lower\n`.

## `docker export` Drops Container Writes

Priority: P1
Impact: silent data loss in exported tar streams
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-3b-worktree`.

Evidence:

- Export resolves `c.rootfs.clone()` instead of the merged/container state: `dd-daemon/src/containers/inspect/admin.rs:48`.
- The tar command streams from that lower rootfs: `dd-daemon/src/containers/inspect/admin.rs:53`.

Why this is bad:

`docker export` should export the container filesystem, not the original image rootfs. Exporting stale lower content makes backups and migration silently wrong.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-target-3b cargo test -p dd-daemon deeper_3b -- --ignored --nocapture
```

PoC `docker export` case expected `upper\n` but read `lower\n`.

## Dockerfile Runtime Metadata Is Accepted But Dropped

Priority: P2
Impact: image config mismatch and runtime behavior drift
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-3b-worktree`.

Evidence:

- The build handler explicitly ignores `EXPOSE`, `USER`, `VOLUME`, and `HEALTHCHECK`: `dd-daemon/src/build/handler.rs:383`.
- The final sidecar config omits those fields: `dd-daemon/src/build/handler.rs:458`.

Why this is bad:

Dockerfiles can build successfully while runtime-affecting metadata disappears. For example, `USER 1001` should affect default runtime identity and image inspection; current output stores an empty user.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-target-3b cargo test -p dd-daemon deeper_3b -- --ignored --nocapture
```

PoC `USER 1001` case produced image user `""`.

## Failed Start Leaves A Spent `Live`

Priority: P1
Impact: start API can report success without spawning a process
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-3b-worktree`.

Evidence:

- Start reuses an existing `Live` entry: `dd-daemon/src/containers/lifecycle/run.rs:23`.
- Start marks the container running before spawn: `dd-daemon/src/containers/lifecycle/run.rs:29`.
- Spawn failure records exit state on the `Live` but does not remove the spent entry: `dd-daemon/src/runtime/spawn/live.rs:337`.

Why this is bad:

After one failed start, a second start can return `204` through a stale `Live` path without actually spawning. That creates false running state and confusing logs/events.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-target-3b cargo test -p dd-daemon deeper_3b -- --ignored --nocapture
```

PoC second start returned `204` again after the first failed spawn.

## `docker top` Returns Fake Processes For Stopped Containers

Priority: P2
Impact: container state inspection lies
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-3b-worktree`.

Evidence:

- `containers_top` resolves the container but never checks running state: `dd-daemon/src/containers/inspect/top.rs:5`.
- It always returns a synthetic PID 1 row: `dd-daemon/src/containers/inspect/top.rs:15`.

Why this is bad:

Docker returns a conflict for `top` on a stopped container. A synthetic process row makes stopped containers look alive to orchestration code.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-target-3b cargo test -p dd-daemon deeper_3b -- --ignored --nocapture
```

PoC expected `409` but received `200`.

## Concurrent Pulls Share A Layer Temp File

Priority: P2
Impact: pull races can corrupt or delete another extraction input
Confidence: High

Evidence:

- Layer unpack temp path uses only process id and layer id: `dd-images/src/registry/client/pull.rs:99`.
- The temp file is removed unconditionally after extraction: `dd-images/src/registry/client/pull.rs:123`.

Why this is bad:

Two pulls of the same layer in one daemon process use the same temporary path. One pull can truncate, replace, or remove the other pull's layer input, producing extraction failures or corrupted rootfs state under concurrency.

Verification:

Run two concurrent pulls of images sharing a layer inside one daemon and trace `dd-layer-<pid>-<layer-id>.tar.gz`. Expected fix behavior: each unpack uses a unique temp path or serializes per layer digest.

## Build-Cache Layer Replacement Is Non-Atomic

Priority: P1
Impact: failed replacement or prune race can delete a good cache layer
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-workerC-daemon-storage-20260710`.

Evidence:

- `store_layer` removes the existing layer dir before copying the replacement: `dd-images/src/build/cache.rs:271`.
- `build_prune` removes layer dirs directly without coordination: `dd-daemon/src/build/prune.rs:15`.

Why this is bad:

If the replacement snapshot fails after deletion, or a prune races a build, a previously valid cache entry disappears. That can cause avoidable rebuilds or later cache restore failures.

Isolated proof:

```sh
cargo test -p dd-images store_layer_failed_replacement_preserves_existing_layer -- --nocapture
```

Result: failed because the failed replacement deleted the existing cache layer.

## Stats Stream Captures A Stale Pid

Priority: P2
Impact: stale or wrong process stats after exit/restart
Confidence: Medium

Evidence:

- `containers_stats` captures the live pid once: `dd-daemon/src/containers/inspect/stats.rs:142`.
- The stream then samples for up to 3600 iterations using captured state: `dd-daemon/src/containers/inspect/stats.rs:175`.

Why this is bad:

If the container exits or restarts while a stats stream is open, the stream can keep reporting stale data. If the host reuses the pid, stats can describe the wrong process.

Verification:

Start a stats stream, restart the container, and assert the stream either ends or switches to the new live pid with clear semantics.

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

## Exec Start Is Not Single-Use

Priority: P1
Impact: duplicate exec processes and detached lifecycle state
Confidence: High

Evidence:

- Exec start inserts a fresh `Live` under the exec id: `dd-daemon/src/containers/exec/start.rs:63`.
- It sets `Exec.started` but does not reject a later start: `dd-daemon/src/containers/exec/start.rs:65`.
- `spawn_live` is idempotent only per `Live` object: `dd-daemon/src/runtime/spawn/live.rs:12`.
- Exec reaper removes `g.live[cid]` for that exec id on exit: `dd-daemon/src/runtime/spawn/live.rs:277`.

Why this is bad:

A second `/exec/:id/start` can create a second process and replace live state for the same exec id. The first process can later remove the second process's live entry, making inspect/control wrong.

Verification:

Start one exec id twice with a long-running command and assert Docker-compatible rejection or single-use behavior.

## Events Are Live-Only And Lossy

Priority: P2
Impact: clients relying on replay or complete event sequences miss lifecycle events
Confidence: High

Evidence:

- Events are dropped immediately when there are no receivers: `dd-daemon/src/events.rs:39`.
- `since` is accepted but unused: `dd-daemon/src/events.rs:79`.
- Lagged broadcast receivers skip events and continue: `dd-daemon/src/events.rs:223`.

Why this is bad:

`docker events --since ...` should be able to replay prior events over a time window. dd keeps no history and can silently skip live events for lagging clients.

Verification:

Emit lifecycle events before subscribing with `since=<past>` and assert they are replayed, or document and hard-fail unsupported replay.

## `logs -f` Can Drop Output For Slow Clients

Priority: P2
Impact: silent live log truncation
Confidence: Medium-high

Evidence:

- Live output uses a bounded broadcast channel: `dd-daemon/src/model/state.rs:33`.
- Follow mode relays through a bounded mpsc channel: `dd-daemon/src/containers/inspect/logs.rs:152`.
- Lagged broadcast receive is skipped with `continue`: `dd-daemon/src/containers/inspect/logs.rs:188`.

Why this is bad:

If a client or response channel backpressures while a container produces output quickly, the follow task can lag and skip chunks. Non-follow logs may still have buffered data, but the live `logs -f` stream loses it.

Verification:

Run a high-output container with a deliberately slow `logs -f` consumer and compare live stream bytes against buffered logs after exit.

## Healthcheck `NONE` Create Override Makes Fake Health

Priority: P2
Impact: disabled healthchecks still produce `State.Health`
Confidence: High

Evidence:

- Image-pull config normalizes `Test=["NONE"]` to `None`: `dd-daemon/src/images/pull/config.rs:174`.
- Container create stores the override verbatim: `dd-daemon/src/containers/lifecycle/create/mod.rs:251`.
- `spawn_live` starts a monitor for any `Some` healthcheck: `dd-daemon/src/runtime/spawn/live.rs:187`.
- The probe treats `NONE` as success: `dd-daemon/src/runtime/health.rs:23`.

Why this is bad:

Docker `Healthcheck: {"Test":["NONE"]}` disables healthchecks. dd can instead run a monitor that reports fake healthy state.

Verification:

Create a container with `Healthcheck.Test=["NONE"]`, start it, and assert inspect has no `State.Health`.

## Stop Timeout Marks Exited Before Reaper Confirms Death

Priority: P2
Impact: rm/restart/port reuse can race a still-running process
Confidence: Medium

Evidence:

- Stop timeout sends `SIGKILL` and breaks the wait loop: `dd-daemon/src/containers/mod.rs:92`.
- It then frees ports and marks the container exited before the reaper confirms process death: `dd-daemon/src/containers/mod.rs:102`.

Why this is bad:

`SIGKILL` is reliable but not synchronous. Marking exited and freeing resources before reaping can race remove, restart, port reuse, and exit-code reporting.

Verification:

Use a process group with delayed teardown or observable child cleanup, force stop timeout, and assert state/resource release waits for reaper confirmation.

## Tar Extraction Trust Boundaries

Priority: P2
Impact: possible host/rootfs path traversal or symlink escape
Confidence: Medium

Evidence:

- Build context extraction shells out to `tar xf`: `dd-daemon/src/build/handler.rs:51`.
- `docker cp` put extracts client tar into a resolved host dir: `dd-daemon/src/archive/handlers.rs:172`.
- `docker load` extracts via host `tar`: `dd-images/src/image/archive/load.rs:21`.
- `docker import` extracts via host `tar`: `dd-images/src/image/archive/import.rs:19`.

Why this is suspicious:

Archive extraction safety depends on system `tar` behavior and target directory state. Malicious members with `..`, absolute paths, or symlink chains can escape intended extraction roots in many naive archive flows.

Verification:

Send crafted tar archives with traversal, absolute paths, and symlink-follow cases to build, load/import, and container archive endpoints. Assert post-extract containment.

## Perf and Bench Gates Can Lie

Priority: P1
Impact: performance regressions, hangs, and missing dd lanes can be reported as acceptable benchmark output
Confidence: High

Verification status: Proven with failing guard tests in isolated worktree `/Users/x/dd/dd-agent4`.

Evidence:

- Perf reruns time JIT invocations while discarding status/output: `dd-tests/src/harness/perf.rs:166`.
- Timed perf reruns lack the normal timeout wrapper: `dd-tests/src/harness/perf.rs:166`.
- `BENCH_N=0` is accepted, reaching empty-sample median behavior: `dd-tests/src/bin/bench.rs:145`.
- Missing dd bench lanes only warn and write blank dd columns: `dd-tests/src/bin/bench.rs:188`, `dd-tests/src/bin/bench.rs:219`.
- Bench artifact write failures are ignored: `dd-tests/src/bin/bench.rs:305`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-agent4-target cargo test -p dd-tests --test gate_invariants -- --nocapture
```

The guard suite includes failing tests:

- `perf_matrix_rechecks_timed_invocation_success`
- `perf_matrix_hang_guard_wraps_timed_jit_runs`
- `bench_rejects_zero_repetitions`
- `bench_fails_when_dd_lanes_are_missing`
- `bench_persist_reports_write_failures`

Why this is bad:

CI can pass when an engine lane is unavailable, a matrix is mostly skipped, or a known gap unexpectedly passes but remains marked xfail.

Verification:

Run the cargo-test path with one or more engines unavailable and inspect whether the suite can pass skip-only or xpass-containing results.

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

## `DDOCKERD_SOCK` Startup Unlinks Configured Path

Priority: P2
Impact: environment-only daemon config can delete the wrong socket path
Confidence: Medium-high

Evidence:

- Daemon reads `DDOCKERD_SOCK`: `dd-daemon/src/main.rs:66`.
- It unconditionally removes that path before binding: `dd-daemon/src/main.rs:71`.
- Binding occurs later at the configured path: `dd-daemon/src/main.rs:131`.

Why this is bad:

An env-var-only socket override should not blindly unlink arbitrary configured paths. At minimum, startup should verify the path is a stale socket owned by the daemon before removing it.

## Inspect Network Endpoint JSON Is Sparse

Priority: P2
Impact: Docker API clients miss expected endpoint fields
Confidence: Medium

Evidence:

- Container inspect serializes a reduced endpoint shape: `dd-daemon/src/api/container/inspect.rs:132`.
- The wire endpoint model has only name and IP: `dd-daemon/src/model/wire/network.rs:7`.

Why this is bad:

Docker inspect consumers commonly read endpoint id, gateway, aliases, MAC address, and IPAM fields. Sparse JSON can break clients that use inspect output for network inventory or service discovery.

## `HostConfig.AutoRemove` Is Omitted From Inspect

Priority: P2
Impact: inspect clients see incomplete lifecycle configuration
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-Y-daemon-api-20260710`.

Evidence:

- Create DTO accepts `HostConfig.AutoRemove`: `dd-daemon/src/containers/lifecycle/create/dto.rs:98`.
- Container model persists auto-remove state: `dd-daemon/src/model/wire/container.rs:113`.
- Inspect omits the field from `HostConfig`: `dd-daemon/src/api/container/inspect.rs:102`.
- Exit teardown acts on auto-remove: `dd-daemon/src/containers/inspect/detail.rs:113`.

Why this is bad:

The daemon honors auto-remove behavior but hides it from inspect output. Diff tools and orchestration clients can see `null` or missing state for a setting that will later delete the container.

Isolated proof:

```sh
CARGO_TARGET_DIR=target-worker-Y-daemon-api-20260710 cargo test -p dd-daemon containers_inspect_reports_hostconfig_auto_remove -- --nocapture
```

Result: failed; `HostConfig.AutoRemove` was `Null`, expected `true`.

## Fast-Exit Event Ordering Can Emit `die` Before `start`

Priority: P2
Impact: event consumers can observe impossible lifecycle order
Confidence: Medium

Evidence:

- Start emits event state around launch: `dd-daemon/src/containers/lifecycle/run.rs:40`.
- Reaper/die event emission happens from live lifecycle code: `dd-daemon/src/runtime/spawn/live.rs:202`.

Why this is suspicious:

For very short-lived containers, the reaper path can race the start event path. Event consumers expect `start` before `die`; inverted ordering can corrupt state machines that replay event streams.

Verification:

Run a fast-exiting container repeatedly under event capture and assert each container's event order is create, start, die.

## Daemon Restart Reloads Running Containers Without Live Process

Priority: P1
Impact: persisted state can report running while no process exists
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AA-daemon-image-20260710`.

Evidence:

- State reload reads persisted model directly: `dd-daemon/src/util/state.rs:28`.
- Start returns not-modified for containers already marked running: `dd-daemon/src/containers/lifecycle/run.rs:20`.
- Inspect derives running state and pid from model/live state: `dd-daemon/src/containers/inspect/detail.rs:14`.

Why this is bad:

After daemon restart, no live process is attached. If persisted status remains running, inspect can report `Running=true` with `Pid=0`, and `POST /start` can become a no-op instead of reconciling or restarting the container.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AA-daemon-image-20260710/target-aa pocs/slot-aa/reload-running-no-live.sh
```

Observed: `State.Status=running`, `State.Running=true`, `State.Pid=0`, and start returned `304`.

## Container Prune Leaves Network Endpoints

Priority: P1
Impact: pruned containers can leave user networks undeletable
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AA-daemon-image-20260710`.

Evidence:

- Container prune removes container state in admin code: `dd-daemon/src/containers/inspect/admin.rs:15`.
- Container remove has explicit network cleanup: `dd-daemon/src/containers/lifecycle/manage.rs:156`.
- Network delete rejects networks with endpoints: `dd-daemon/src/networks/handlers.rs:88`.

Why this is bad:

Prune should be equivalent to removing eligible containers. Leaving network endpoints behind makes network inspect show deleted containers and can make user networks impossible to delete.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AA-daemon-image-20260710/target-aa pocs/slot-aa/prune-leaves-network-endpoint.sh
```

Observed: prune deleted the exited container, but `network inspect` still listed its endpoint and deleting the network returned `403`.

## `rmi nginx` Removes Unrelated Repositories Sharing Basename

Priority: P1
Impact: image delete can remove unrelated repository tags
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AA-daemon-image-20260710`.

Evidence:

- Image tag deletion resolves aliases through basename-style matching: `dd-daemon/src/images/tags.rs:64`.
- Deletion applies to matching references later in the same module: `dd-daemon/src/images/tags.rs:88`.
- The basename helper is explicitly risky for repository identity: `dd-images/src/image/config.rs:100`.

Why this is bad:

`nginx:latest` and `linuxserver/nginx:latest` are distinct repositories. Deleting the short library name should not delete unrelated images that happen to share the same basename and tag.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AA-daemon-image-20260710/target-aa pocs/slot-aa/rmi-basename-removes-other-repos.sh
```

Observed: before delete `['linuxserver/nginx:latest', 'nginx:latest']`; after `DELETE /images/nginx`, no tags remained.

## Failed Spawn Terminal State Is Not Persisted

Priority: P1
Impact: daemon restart can resurrect failed starts as running
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AD-daemon-api-20260710`.

Evidence:

- Failed live spawn sets status to `exited` and exit code `127`: `dd-daemon/src/runtime/spawn/live.rs:338`, `dd-daemon/src/runtime/spawn/live.rs:348`.
- The normal reaper path persists state: `dd-daemon/src/runtime/spawn/live.rs:215`.
- The failed-spawn path does not call the same state save path.

Why this is bad:

A failed start can be corrected in memory but not written to disk. After daemon restart, persisted state can still say `running`, so inspect and wait paths see a live-looking container with no process.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AD-daemon-api-20260710-target cargo test -p dd-daemon --bin dd-daemon live_fail_persists_terminal_exit_state -- --nocapture
```

Result: failed; reloaded state remained `running`, expected `exited` with exit code `127`.

## `docker tag` Aliases Do Not Survive Discovery

Priority: P1
Impact: image aliases vanish after daemon restart
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AG-daemon-image-20260710`.

Evidence:

- Tagging mutates only in-memory image state: `dd-daemon/src/images/tags.rs:37`.
- Restart discovery rebuilds images from on-disk sidecars: `dd-images/src/image/discovery/mod.rs:63`.

Why this is bad:

Tags are persistent Docker image metadata. If aliases exist only in daemon memory, a restart or rediscovery can drop them, breaking later `run`, `push`, or delete operations that refer to the alias.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AG-daemon-image-20260710/target-ag cargo test -p dd-daemon image_tag_alias_survives_daemon_restart_discovery -- --ignored --nocapture
```

Result: failed; alias was visible before simulated restart but missing from `discover_images`.

## Retained Container Logs Are Lost Across Daemon Restart

Priority: P1
Impact: `docker logs` can lose exited-container output after restart
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-state-ai`.

Evidence:

- Container stdout/stderr fields are skipped during state serialization: `dd-daemon/src/model/wire/container.rs:155`.
- Logs fall back to retained container stdout/stderr after live log chunks disappear: `dd-daemon/src/containers/inspect/logs.rs:87`.
- Daemon state reload persists only serialized state: `dd-daemon/src/util/state.rs:6`.

Why this is bad:

Exited container logs are expected to survive daemon restarts. dd can return an empty body after reload because both the live log buffer and skipped retained fields are gone.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-state-ai/target-audit cargo test -p dd-daemon audit_logs_survive_state_reload_for_exited_container -- --ignored --nocapture
```

Result: failed; logs body was empty, expected `hello after restart`.

## Failed Spawn Leaks Published Host-Port Forwarders

Priority: P1
Impact: failed starts can leave host ports bound
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-state-ai`.

Evidence:

- `spawn_live` starts published port forwarders before spawn completion: `dd-daemon/src/runtime/spawn/live.rs:137`, `dd-daemon/src/runtime/spawn/live.rs:172`.
- Failed spawn handling marks exit state but does not stop forwarders: `dd-daemon/src/runtime/spawn/live.rs:338`.
- Port forwarder stop logic exists separately: `dd-daemon/src/containers/ports.rs:115`.

Why this is bad:

A failed container start should release every resource acquired during startup. Leaking the forwarder keeps the host port bound, causing later starts to fail or route traffic to no live container.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-state-ai/target-audit cargo test -p dd-daemon audit_live_fail_releases_published_port_forwarder -- --ignored --nocapture
```

Result: failed; failed spawn left `127.0.0.1:42551` bound.

## Container Prune Deletes Without Destroy Events

Priority: P2
Impact: event consumers miss container deletions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-state-ai`.

Evidence:

- Container prune removes state from admin code: `dd-daemon/src/containers/inspect/admin.rs:7`.
- Explicit remove emits `container/destroy`: `dd-daemon/src/containers/lifecycle/manage.rs:130`.

Why this is bad:

Consumers that mirror daemon state from events need a destroy event for pruned containers. Silent prune deletion leaves caches and UIs with stale containers.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-state-ai/target-audit cargo test -p dd-daemon audit_container_prune_emits_destroy_event -- --ignored --nocapture
```

Result: failed; the test timed out waiting for a `container/destroy` event.

## Natural Container Exit Leaves Published Host Ports Bound

Priority: P1
Impact: exited containers can keep host ports occupied
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AN-daemon-state-20260710`.

Evidence:

- Reaper marks non-restarting containers exited and emits `die`: `dd-daemon/src/runtime/spawn/live.rs:217`.
- Port forwarders are stopped only in the `AutoRemove` branch: `dd-daemon/src/runtime/spawn/live.rs:287`.
- Explicit stop/remove cleanup uses port stop logic elsewhere: `dd-daemon/src/containers/ports.rs:95`.

Why this is bad:

When a non-restarting container exits naturally, its published ports should be released. Keeping the forwarder bound prevents later containers from using the port and can route traffic to a dead service.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AN-target cargo test -p dd-daemon audit_exited_container_forwarder_releases_host_port_without_explicit_stop -- --nocapture
```

Result: failed; naturally exited container left host port `44931` bound.

## Network Prune Deletes Without Destroy Events

Priority: P2
Impact: event consumers miss network deletions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AN-daemon-state-20260710`.

Evidence:

- Network prune retains/removes networks and saves state but emits no event: `dd-daemon/src/networks/handlers.rs:176`.

Why this is bad:

Docker event consumers expect lifecycle events for pruned objects. Silent network prune leaves watchers with stale network inventory.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AN-target cargo test -p dd-daemon audit_network_prune_emits_destroy_events_for_pruned_networks -- --nocapture
```

Result: failed; event list was empty.

## Volume Prune Deletes Without Destroy Events

Priority: P2
Impact: event consumers miss volume deletions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AN-daemon-state-20260710`.

Evidence:

- Volume prune deletes volume dirs and saves state but emits no event: `dd-daemon/src/volumes.rs:149`.

Why this is bad:

Volume lifecycle listeners can keep stale references to pruned volumes because no `volume/destroy` event is emitted.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AN-target cargo test -p dd-daemon audit_volume_prune_emits_destroy_events_for_pruned_volumes -- --nocapture
```

Result: failed; event list was empty.

## Network Connect/Disconnect Mutate Endpoints Without Events

Priority: P2
Impact: event consumers miss endpoint attach/detach changes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710`.

Evidence:

- Network connect saves state and returns OK without emitting a `network/connect` event: `dd-daemon/src/networks/handlers.rs:131`.
- Network disconnect saves state and returns OK without emitting a `network/disconnect` event: `dd-daemon/src/networks/handlers.rs:155`.

Why this is bad:

Network inventory watchers rely on events to mirror endpoint membership. Silent connect/disconnect changes leave those mirrors stale until they poll inspect.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710-target cargo test -p dd-daemon audit_network_connect_disconnect_emit_events -- --ignored --nocapture
```

Result: failed with no network connect/disconnect events observed.

## Image Prune Is A Hard-Coded No-Op

Priority: P2
Impact: dangling image rootfs/state cannot be reclaimed through prune
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710`.

Evidence:

- Image prune returns an empty `ImagesDeleted` list and does not take daemon state: `dd-daemon/src/images/query.rs:62`.
- Dangling images can be produced by commit without a repository: `dd-daemon/src/build/prune.rs:122`.

Why this is bad:

`docker image prune` should reclaim dangling images and report deleted references. A hard-coded no-op leaves disk usage and image inventory stale while reporting success.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710-target cargo test -p dd-daemon audit_image_prune_removes_dangling_images -- --ignored --nocapture
```

Result: failed; prune report was empty for a dangling image.

## Container Rename Updates State Without Event

Priority: P3
Impact: event-stream mirrors miss name changes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710`.

Evidence:

- Rename changes `c.name`, saves state, and returns `204` without emitting `container/rename`: `dd-daemon/src/containers/lifecycle/manage.rs:21`.

Why this is bad:

Event-stream consumers cannot learn the new name without polling inspect, leaving caches and UIs stale after rename.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710-target cargo test -p dd-daemon audit_container_rename_emits_rename_event -- --ignored --nocapture
```

Result: failed with no container rename event observed.

## Forced `rmi` Deletes Rootfs Referenced By Containers

Priority: P1
Impact: existing containers can lose their backing rootfs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710`.

Evidence:

- Forced image removal bypasses in-use conflict checks: `dd-daemon/src/images/tags.rs:75`.
- Last-reference deletion removes the on-disk image dir: `dd-daemon/src/images/tags.rs:91`, `dd-daemon/src/images/tags.rs:96`.

Why this is bad:

Containers keep `Container.rootfs` paths pointing into image storage. Forced image removal may remove tags, but it must not delete backing storage while existing containers still reference it; otherwise restart/export/diff/commit can break.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710/target-ax cargo test -p dd-daemon audit_forced_rmi_preserves_container_backing_rootfs -- --ignored --nocapture
```

Result: failed because `rootfs/bin/sh` was deleted.

## Non-Forced `rmi` Can Delete Rootfs Used Through Alias

Priority: P1
Impact: alias removal can delete backing storage still used by containers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710`.

Evidence:

- In-use checks compare only the tag being deleted: `dd-daemon/src/images/tags.rs:74`.
- Last-reference deletion removes the rootfs: `dd-daemon/src/images/tags.rs:91`.

Why this is bad:

A container can reference an image/rootfs through an older alias while the current last tag points to the same storage. Non-forced `rmi` should conflict if any container still references that rootfs, not delete the storage.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710-target cargo test -p dd-daemon image_rmi_last_alias_refuses_when_container_uses_same_rootfs_under_old_tag -- --nocapture
```

Result: failed; observed `status=200 OK, rootfs_survived=false`, expected `409 CONFLICT` and preserved rootfs.

## Docker Load Of Same Tag Rewrites Existing Container Rootfs

Priority: P1
Impact: existing containers can observe a different lower filesystem after image load
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BH2-daemon-image-ref-config-state-20260710`.

Evidence:

- Container create persists the image rootfs path: `dd-daemon/src/containers/lifecycle/create/mod.rs:255`.
- Load chooses a deterministic tag directory: `dd-images/src/image/archive/mod.rs:68`.
- Load removes that target before installing replacement rootfs: `dd-images/src/image/archive/load.rs:57`.
- Loaded image is registered with that rootfs path: `dd-daemon/src/images/transfer/load.rs:22`.
- Existing in-memory tag is replaced by repo tag: `dd-daemon/src/images/tags.rs:135`.

Why this is bad:

Container lower filesystems should represent the image snapshot used at create time. Loading a replacement for the same tag should not mutate existing containers' lower rootfs contents.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-BH2-daemon-image-ref-config-state-20260710-target cargo test -p dd-daemon image_load_same_tag_does_not_mutate_existing_container_lower_rootfs -- --nocapture
```

Result: failed; existing container rootfs marker changed from `old` to `new`.

## Create Env Overrides Remain Duplicated In Config

Priority: P2
Impact: inspect/state can expose stale image env values
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BH2-daemon-image-ref-config-state-20260710`.

Evidence:

- Create appends request env to image env without dedup: `dd-daemon/src/containers/lifecycle/create/mod.rs:62`.
- Stored container env is the raw appended list: `dd-daemon/src/containers/lifecycle/create/mod.rs:285`.
- Inspect returns that raw list as `Config.Env`: `dd-daemon/src/containers/inspect/detail.rs:99`.
- Launch later dedups last-wins: `dd-jit/src/runtime/container/env.rs:7`.

Why this is bad:

Runtime and API state diverge. The guest may see last-wins env, while inspect/state consumers still see stale image values such as both `FOO=image` and `FOO=run`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-BH2-daemon-image-ref-config-state-20260710-target cargo test -p dd-daemon container_create_env_override_replaces_image_env_key_in_config_state -- --nocapture
```

Result: failed with `["FOO=image", "BAR=base", "FOO=run"]`; expected one effective `FOO=run`.

## Container Create Drops Inherited Image Labels

Priority: P2
Impact: containers lose image label metadata unless repeated in request
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710`.

Evidence:

- Create inherits image env/workdir/user: `dd-daemon/src/containers/lifecycle/create/mod.rs:62`.
- Container labels are set only from request labels: `dd-daemon/src/containers/lifecycle/create/mod.rs:287`.

Why this is bad:

Docker containers inherit image labels, with create-body labels overriding same-key image labels. Dropping inherited labels breaks metadata selectors and inspection expectations.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710-target cargo test -p dd-daemon container_create_inherits_image_labels_and_applies_overrides -- --nocapture
```

Result: failed; `com.example.inherited` was missing.

## Daemon Discovery Drops Image Labels

Priority: P2
Impact: image labels disappear after daemon restart/discovery
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710`.

Evidence:

- Daemon image model has labels: `dd-daemon/src/model/wire/image.rs:16`.
- `DiscoveredImage` has no labels field: `dd-images/src/image/discovery/mod.rs:19`.
- Discovery mapping defaults labels away: `dd-daemon/src/util/discover.rs:29`.

Why this is bad:

Labels stored in `dd-image.json` should survive daemon discovery/restart. Dropping them on reload breaks inspect, filters, and metadata-dependent workflows.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710-target cargo test -p dd-daemon discover_images_preserves_sidecar_labels -- --nocapture
```

Result: failed; label lookup returned `None`.

## Image Event Filters Drop Image Events

Priority: P2
Impact: filtered event streams miss matching image lifecycle events
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710`.

Evidence:

- Event filter matching checks `Actor.Attributes.image`: `dd-daemon/src/events.rs:123`, `dd-daemon/src/events.rs:138`.
- Image lifecycle events publish `Actor.Attributes.name`: `dd-daemon/src/images/tags.rs:102`, `dd-daemon/src/images/pull/stream.rs:85`.

Why this is bad:

`docker events --filter type=image --filter image=busy:1` should select matching image events. dd emits the name under a different attribute key, so filtered streams can silently drop the event.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710/target-ax cargo test -p dd-daemon audit_image_events_match_image_filter_by_name -- --ignored --nocapture
```

Result: failed with an empty response body.

## `POST /system/prune` Is Not Routed

Priority: P2
Impact: Docker-compatible clients receive 404 for system prune
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710`.

Evidence:

- Router exposes individual prune endpoints but no `/system/prune`: `dd-daemon/src/routes.rs:21`.

Why this is bad:

Docker clients use `/system/prune` for combined cleanup. Missing the route makes compatible clients hit fallback 404 even though related prune endpoints exist.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710/target-ax cargo test -p dd-daemon audit_system_prune_route_is_exposed -- --ignored --nocapture
```

Result: failed with status `404`.

## `docker commit` Can Inherit Config From Wrong Repository

Priority: P1
Impact: committed images can silently get wrong entrypoint/config
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AL-daemon-image-20260710`.

Evidence:

- Commit resolves the source image with basename-style `ref_name`: `dd-daemon/src/build/prune.rs:85`.

Why this is bad:

A container created from `linuxserver/nginx:latest` should inherit that image config during commit. Basename matching can instead find `nginx:latest` and copy the wrong entrypoint or runtime metadata.

Isolated proof:

```sh
TMPDIR="$PWD/target/tmp" cargo test -p dd-daemon flow_commit_source_image_lookup_preserves_full_repository_identity -- --nocapture
```

Result: failed; committed config had `["/wrong-entrypoint"]`, expected `["/right-entrypoint"]`.

## Committed ELF-Less x86_64 Images Rediscover As arm64

Priority: P1
Impact: daemon restart can switch engine architecture for scratch-style images
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AL-daemon-image-20260710`.

Evidence:

- Commit writes `dd-image.json` without arch/os: `dd-daemon/src/build/prune.rs:195`.
- Discovery falls back to probing and then `LinuxAarch64`: `dd-images/src/image/discovery/mod.rs:73`.

Why this is bad:

Scratch or distroless-style committed x86_64 images may lack an ELF for discovery. If architecture is not persisted, restart discovery can relabel the image as arm64 and select the wrong engine.

Isolated proof:

```sh
TMPDIR="$PWD/target/tmp" cargo test -p dd-daemon flow_commit_x86_elfless_image_roundtrips_arch_through_discovery -- --nocapture
```

Result: failed; rediscovered arch was `LinuxAarch64`, expected `LinuxX86_64`.

## Daemon Save/Load Drops Image Labels

Priority: P2
Impact: saved images lose label metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AV-archive-load-save-push-registry-20260710`.

Evidence:

- Daemon image save builds a manifest subset: `dd-daemon/src/images/transfer/save.rs:38`.
- The archive manifest type has no labels field: `dd-images/src/image/manifest.rs:15`.
- Load reconstructs daemon image state from that manifest: `dd-daemon/src/images/transfer/load.rs:22`.
- Daemon image model tracks labels: `dd-daemon/src/model/wire/image.rs:16`.

Why this is bad:

Image labels are application and OCI metadata. Save/load should preserve them; dropping labels silently can break deployment metadata, selectors, and provenance checks.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AV-archive-load-save-push-registry-20260710-target cargo test -p dd-daemon poc_save_load_preserves_image_labels -- --ignored --nocapture
```

Result: failed; labels reloaded as `{}` instead of the original map.

## Docker Save/Load Corrupts ELF-Less Linux x86 Images To arm64

Priority: P1
Impact: save/load can switch image engine architecture
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710`.

Evidence:

- Daemon save writes `os` only for Darwin and omits Linux architecture: `dd-daemon/src/images/transfer/save.rs:38`.
- Load falls back to ELF sniffing and then `LinuxAarch64`: `dd-images/src/image/archive/load.rs:64`.

Why this is bad:

Scratch or distroless linux/amd64 rootfs trees may not contain an ELF binary to sniff. Save/load should carry `linux/amd64`; otherwise the restored image can run under the wrong engine.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710/target-aq cargo test -p dd-daemon image_save -- --nocapture
```

Result: failed with restored arch `LinuxAarch64`, expected `LinuxX86_64`.

## `docker save nginx` Can Serialize `linuxserver/nginx`

Priority: P1
Impact: save can export the wrong repository image
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710`.

Evidence:

- `image_save` matches exact `repo_tag`, then falls back to basename-only `ref_name`: `dd-daemon/src/images/transfer/save.rs:20`.

Why this is bad:

Short official names must not match unrelated repositories with the same basename. Saving `nginx` can serialize `linuxserver/nginx:latest`, corrupting backups and transfers.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710/target-aq cargo test -p dd-daemon image_save -- --nocapture
```

Result: failed; short `nginx` returned success for unrelated `linuxserver/nginx`, expected not found.

## Restart State Load Overwrites Persisted Container Arch

Priority: P2
Impact: unresolved images can change persisted container architecture
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710`.

Evidence:

- State load forces unresolved images to `Guest::LinuxAarch64`: `dd-daemon/src/util/state.rs:49`.

Why this is bad:

Persisted container state may already contain the correct architecture. If image discovery misses the referenced image, reload should preserve that arch instead of defaulting to arm64.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710/target-aq cargo test -p dd-daemon load_state_preserves_persisted_arch_when_image_absent -- --nocapture
```

Result: failed with `Some(LinuxAarch64)`, expected `Some(LinuxX86_64)`.

## Container Inspect Collapses Entrypoint And Cmd

Priority: P1
Impact: inspect output loses Docker-shaped runtime config fields
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BN2-copy`.

Evidence:

- Create resolves image entrypoint plus command into one launch argv: `dd-daemon/src/containers/lifecycle/create/mod.rs:61`.
- Container state stores `working_dir`, `env`, and `user`: `dd-daemon/src/containers/lifecycle/create/mod.rs:284`.
- Inspect emits only `cmd`, `hostname`, `image`, `env`, labels, health, and stop signal: `dd-daemon/src/containers/inspect/detail.rs:99`.
- The API `ContainerConfig` has no `Entrypoint`, `WorkingDir`, or `User`: `dd-daemon/src/api/container/inspect.rs:88`.

Why this is bad:

Docker inspect preserves split `Config.Entrypoint` and `Config.Cmd`, plus `Config.WorkingDir` and `Config.User`. Collapsing the launch argv and omitting fields breaks tools that reconstruct runtime defaults from inspect output.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-BN2-copy
cargo test -p dd-daemon audit_container_inspect_preserves_split_runtime_config -- --ignored --nocapture
```

Result: failed with `Config.Cmd` observed as `["/entry", "--serve"]`, expected `["--serve"]`; the missing fields should report `Entrypoint=["/entry"]`, `WorkingDir="/srv/app"`, and `User="1001:1002"`.

## Docker Commit Drops Container User

Priority: P1
Impact: committed images lose their default runtime user
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BN2-copy`.

Evidence:

- Commit collects rootfs, cmd, entrypoint, env, workdir, labels, and arch, but not `c.user`: `dd-daemon/src/build/prune.rs:77`.
- The persisted `dd-image.json` omits user: `dd-daemon/src/build/prune.rs:196`.
- The registered committed image omits user and defaults it empty: `dd-daemon/src/build/prune.rs:210`.

Why this is bad:

A committed image should preserve the container's effective `Config.User`. Dropping it silently changes how later containers start from the committed image.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-BN2-copy
cargo test -p dd-daemon audit_commit_preserves_effective_user_in_image_config -- --ignored --nocapture
```

Result: failed with observed user `""`, expected `"1001:1002"`.

## Create `ExposedPorts` Is Dropped From Inspect

Priority: P2
Impact: exposed-but-unpublished ports disappear from container metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-container-state-20260710`.

Evidence:

- Create DTO has no `ExposedPorts` field: `dd-daemon/src/containers/lifecycle/create/dto.rs:8`.
- Create derives published ports only from `HostConfig.PortBindings`: `dd-daemon/src/containers/lifecycle/create/mod.rs:276`.
- Inspect builds `NetworkSettings.Ports` only from published ports: `dd-daemon/src/containers/inspect/detail.rs:150`.

Why this is bad:

Docker create accepts `Config.ExposedPorts` separately from host port bindings. Inspect should preserve `Config.ExposedPorts` and report exposed-but-unpublished ports as null bindings.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-container-state-20260710-target cargo test -p dd-daemon audit_create_exposed_ports_roundtrips_in_inspect -- --nocapture
```

Result: `Config.ExposedPorts["8080/tcp"]` was missing/null.

## Interactive Create Config Is Not Reported

Priority: P2
Impact: interactive container settings are accepted but vanish from inspect
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-container-state-20260710`.

Evidence:

- `CreateBody` reads `Tty` but not `OpenStdin` or `StdinOnce`: `dd-daemon/src/containers/lifecycle/create/dto.rs:20`.
- The persisted model stores only `tty`: `dd-daemon/src/model/wire/container.rs:71`.
- Inspect `Config` omits `Tty`, `OpenStdin`, and `StdinOnce`: `dd-daemon/src/api/container/inspect.rs:88`.

Why this is bad:

Clients use inspect to reconstruct whether a container is interactive. Dropping these fields can break attach/exec tooling and state reconciliation.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-container-state-20260710-target cargo test -p dd-daemon audit_create_interactive_stdio_config_roundtrips_in_inspect -- --nocapture
```

Result: `Config.Tty` was `Null`, expected `true`; `OpenStdin` and `StdinOnce` were also not represented.

## `HostConfig.LogConfig` Is Accepted Then Lost

Priority: P2
Impact: log driver options silently disappear from inspect
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-container-state-20260710`.

Evidence:

- `HostConfig` DTO lacks `LogConfig`: `dd-daemon/src/containers/lifecycle/create/dto.rs:59`.
- Inspect `HostConfigJson` also lacks it: `dd-daemon/src/api/container/inspect.rs:102`.
- Detail population cannot emit it: `dd-daemon/src/containers/inspect/detail.rs:113`.

Why this is bad:

Docker clients can pass `HostConfig.LogConfig` at create time and expect it to round-trip through inspect. Accepting and dropping it hides incompatible logging behavior.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-container-state-20260710-target cargo test -p dd-daemon audit_create_log_config_roundtrips_in_inspect -- --nocapture
```

Result: `HostConfig.LogConfig.Type` was `Null`, expected `json-file`.

## DNS And ExtraHosts Options Are Lost

Priority: P1
Impact: container name resolution settings silently disappear
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-state-lifecycle-src`.

Evidence:

- `HostConfig` DTO lacks `Dns`, `DnsSearch`, `DnsOptions`, and `ExtraHosts`: `dd-daemon/src/containers/lifecycle/create/dto.rs:59`.
- Runtime writes a fixed `/etc/resolv.conf`: `dd-daemon/src/runtime/spawn/live.rs:95`.
- Inspect DTO omits these fields: `dd-daemon/src/api/container/inspect.rs:102`.

Why this is bad:

Docker create accepts DNS and host-entry settings that should affect `/etc/resolv.conf`, `/etc/hosts`, and inspect output. dd accepts the request shape but loses the settings.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-state-lifecycle-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: `HostConfig.Dns` was `Null`; expected `["9.9.9.9"]` plus search/options and `ExtraHosts=["db:10.9.0.2"]`.

## `HostConfig.DeviceRequests` Is Accepted Then Lost

Priority: P2
Impact: GPU/device requests vanish without error
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-state-lifecycle-src`.

Evidence:

- Create DTO supports `Devices` but not `DeviceRequests`: `dd-daemon/src/containers/lifecycle/create/dto.rs:84`.
- Inspect DTO omits `DeviceRequests`: `dd-daemon/src/api/container/inspect.rs:102`.

Why this is bad:

NVIDIA-style device requests should round-trip or be rejected. Dropping them silently makes scheduling/device allocation appear accepted when it is not.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-state-lifecycle-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: `HostConfig.DeviceRequests` was `Null`.

## `HostConfig.NetworkMode` Is Missing From Inspect

Priority: P2
Impact: actual network mode cannot be reconstructed from inspect
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-state-lifecycle-src`.

Evidence:

- Create persists `network_mode`: `dd-daemon/src/containers/lifecycle/create/mod.rs:288`.
- Inspect `HostConfigJson` has no `network_mode` field: `dd-daemon/src/api/container/inspect.rs:102`.

Why this is bad:

Network mode affects start/runtime behavior. Inspect should report `HostConfig.NetworkMode`, especially for `none`, bridge, or container network modes.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-state-lifecycle-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: `HostConfig.NetworkMode` was `Null`, expected `"none"`.

## `Config.Domainname` Is Accepted Then Lost

Priority: P2
Impact: UTS/domain config cannot round-trip
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-state-lifecycle-src`.

Evidence:

- Create DTO has `Hostname` but no `Domainname`: `dd-daemon/src/containers/lifecycle/create/dto.rs:18`.
- Inspect `ContainerConfig` omits `Domainname`: `dd-daemon/src/api/container/inspect.rs:88`.

Why this is bad:

Clients can provide `Domainname` during create and expect it to round-trip or be rejected. dd silently drops it.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-state-lifecycle-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: `Config.Domainname` was `Null`, expected `"example.test"`.

## Endpoint Static IPs And Aliases Are Ignored

Priority: P1
Impact: requested container networking identity is silently replaced
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-netmount-20260710-131246`.

Evidence:

- Create DTO stores endpoint settings as opaque JSON: `dd-daemon/src/containers/lifecycle/create/dto.rs:52`.
- Create uses only endpoint map keys: `dd-daemon/src/containers/lifecycle/create/mod.rs:343`.
- IPAM always auto-allocates IPs: `dd-daemon/src/networks/ipam/alloc.rs:82`.
- Network model has no alias/IPAM fields: `dd-daemon/src/model/wire/network.rs:7`.

Why this is bad:

Docker `NetworkingConfig.EndpointsConfig` can request static IPs and aliases. dd accepts the shape but ignores the requested address and alias metadata, breaking DNS and restart expectations.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-netmount-target cargo test -p dd-daemon --bin dd-daemon create_honors_endpoint_static_ip_and_aliases -- --nocapture
```

Result: requested `172.18.0.77`, stored endpoint IP was `172.18.0.2`.

## Archive PUT Writes Through Read-Only Bind Mounts

Priority: P1
Impact: `docker cp` can mutate read-only host bind sources
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-netmount-20260710-131246`.

Evidence:

- Runtime spawn honors mount `read_only` for the guest: `dd-daemon/src/runtime/spawn/mod.rs:107`.
- Archive overlay converts mounts to `source:target` without flags: `dd-daemon/src/archive/overlay.rs:9`.
- Archive PUT writes into the resolved host path: `dd-daemon/src/archive/handlers.rs:126`.

Why this is bad:

A read-only bind mount should reject writes through `docker cp` / archive PUT. dd's archive path bypasses the read-only flag and writes directly into the host source.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-netmount-target cargo test -p dd-daemon --bin dd-daemon archive_put_rejects_writes_through_readonly_mount -- --nocapture
```

Result: archive PUT returned `200` and created `host/new.txt` containing `new`.

## Bind Mount Propagation Is Dropped

Priority: P2
Impact: bind propagation settings cannot round-trip through create/inspect
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-netmount-20260710-131246`.

Evidence:

- Mount model keeps only `Type`, `Source`, `Target`, and `ReadOnly`: `dd-daemon/src/model/wire/mount.rs:7`.
- Inspect hardcodes bind propagation to `rprivate`: `dd-daemon/src/containers/inspect/mounts.rs:59`.

Why this is bad:

Docker bind mounts can request propagation such as `rshared`. dd drops the requested value and inspect cannot report the effective configuration.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-netmount-target cargo test -p dd-daemon --bin dd-daemon bind_mount_propagation_round_trips_in_inspect -- --nocapture
```

Result: `HostConfig.Mounts[0].BindOptions.Propagation` was `Null`; expected `rshared`.

## Restarting Containers Can Stay Stuck After Daemon Restart

Priority: P1
Impact: restart-policy backoff state can persist without a supervisor
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-20260710`.

Evidence:

- State load restores container status directly: `dd-daemon/src/util/state.rs:28`.
- Daemon startup loads state without recreating sleeping restart-policy supervisors: `dd-daemon/src/main.rs:89`.
- Restart supervisor logic lives separately: `dd-daemon/src/runtime/restart.rs:47`.

Why this is bad:

Reload should reconcile restart-backoff state by resuming restart, marking exited, or normalizing status. dd can preserve `restarting` forever with no task that will actually restart the container.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-target cargo test -p dd-daemon load_state_does_not_preserve_restarting_without_restart_supervisor -- --nocapture
```

Result: failed; status remained `"restarting"`.

## Logs Time Filters Reject RFC3339 Forms

Priority: P2
Impact: Docker-supported `--since` / `--until` inputs disable filtering
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-20260710`.

Evidence:

- Log time parsing splits on `.` and parses integer Unix seconds only: `dd-daemon/src/containers/inspect/frame.rs:7`.

Why this is bad:

Docker supports RFC3339, RFC3339Nano, Unix timestamps, and Go durations for log time filters. dd returns `None` for RFC3339 strings, disabling the filter instead of applying it.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-target cargo test -p dd-daemon parse_unix_ts_accepts_docker_rfc3339_forms -- --nocapture
```

Result: `left: None`, expected `Some(1700000000)`.

## Logs Timestamps Are Second-Precision

Priority: P2
Impact: timestamped logs do not match Docker RFC3339Nano shape
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-20260710`.

Evidence:

- Timestamped log framing emits second precision: `dd-daemon/src/containers/inspect/frame.rs:47`.

Why this is bad:

Docker `logs --timestamps` emits RFC3339Nano-style timestamps with padded fractional nanoseconds. dd emits `2023-11-14T22:13:20Z` instead of `...20.000000000Z`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-target cargo test -p dd-daemon frame_chunk_timestamps_use_docker_rfc3339nano_shape -- --nocapture
```

Result: got `"2023-11-14T22:13:20Z a\n"`.

## Stats JSON Is Internally Inconsistent

Priority: P2
Impact: Docker stats clients see contradictory process and memory data
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-20260710`.

Evidence:

- Stats building reports pids and process counts from different sources: `dd-daemon/src/containers/inspect/stats.rs:117`, `dd-daemon/src/containers/inspect/stats.rs:127`.
- Stats DTO memory shape omits compatibility fields: `dd-daemon/src/api/container/stats.rs:60`.

Why this is bad:

`pids_stats.current` and `num_procs` should not contradict, and Docker-compatible memory stats include fields such as `max_usage` and `failcnt`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-target cargo test -p dd-daemon stats_process_count_and_memory_shape_match_docker_clients -- --nocapture
```

Result: failed first on `num_procs=0` while `pids_stats.current=1`.

## Inspect Can Serialize Contradictory Dead State

Priority: P3
Impact: `State.Status` and `State.Dead` can disagree
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-20260710`.

Evidence:

- Inspect state details derive `Dead` separately from status: `dd-daemon/src/containers/inspect/detail.rs:81`.

Why this is bad:

If daemon state preserves or reloads a dead lifecycle status, `State.Dead` should agree or the status should be normalized. dd can return `State.Status="dead"` with `State.Dead=false`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-target cargo test -p dd-daemon containers_inspect_dead_status_sets_dead_boolean -- --nocapture
```

Result: `State.Dead` was `false`.

## Container Wait Returns Immediately For Created Containers

Priority: P1
Impact: wait clients observe a fake successful exit before the container ever starts
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-exec-health-wait`.

Evidence:

- Container wait falls through to `StatusCode: 0` when there is no live process and no recorded exit: `dd-daemon/src/containers/lifecycle/manage.rs:57`.

Why this is bad:

Docker-compatible wait on a created container should block until the container starts and exits, or until an actual terminal state exists. dd returns immediately with success, so orchestrators can mark never-started containers as completed.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-exec-health-wait-target cargo test -p dd-daemon audit_wait_on_created_container_blocks_until_start_or_exit -- --nocapture
```

Result: returned immediately with `{"StatusCode":0}`.

## Exec Start Does Not Recheck Parent Container State

Priority: P1
Impact: execs can start after the parent stops or pauses between create and start
Confidence: High

Verification status: Source-proven in isolated worktree `/Users/x/dd/dd-audit-daemon-exec-health-wait`.

Evidence:

- Exec create validates parent state up front: `dd-daemon/src/containers/exec/create.rs:38`.
- Exec start later clones parent state without rechecking running/paused status: `dd-daemon/src/containers/exec/start.rs:41`.

Why this is bad:

The container can stop or pause after exec creation. Docker-compatible exec start must validate the current parent state, otherwise stale exec handles can run against a non-running container lifecycle.

## Attach Ignores Stream Selectors

Priority: P1
Impact: attach clients receive or send streams they explicitly disabled
Confidence: High

Verification status: Source-proven in isolated worktree `/Users/x/dd/dd-audit-daemon-exec-health-wait`.

Evidence:

- Attach route does not parse `stdin`, `stdout`, `stderr`, `stream`, or `logs` selectors: `dd-daemon/src/containers/exec/attach.rs:7`, `dd-daemon/src/containers/exec/mod.rs:50`.
- Hijack behavior forwards output and input without selector filtering.

Why this is bad:

Docker attach selectors are part of the API contract. Ignoring them can leak unwanted stdout/stderr into clients, block on stdin unexpectedly, or break clients that attach only to one stream.

## Resize Missing Container Or Exec Reports Success

Priority: P2
Impact: callers cannot distinguish a real resize from a missing target
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-exec-health-wait`.

Evidence:

- Resize handler returns success without checking whether the target container or exec exists: `dd-daemon/src/containers/exec/resize.rs:18`.

Why this is bad:

Terminal resize for a missing container or exec should return a not-found error. dd returns `200`, so clients silently believe a resize was applied to no target.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-exec-health-wait-target cargo test -p dd-daemon audit_resize_missing_container_or_exec_is_404 -- --nocapture
```

Result: observed `200`, expected `404`.

## Exec Inspect Omits Docker State Fields

Priority: P2
Impact: Docker API clients lose exec lifecycle and stream capability details
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-exec-health-wait`.

Evidence:

- Exec inspect returns only `ID`, `Running`, `ExitCode`, `ContainerID`, and `ProcessConfig`: `dd-daemon/src/containers/exec/inspect.rs:23`, `dd-daemon/src/api/exec.rs:18`.

Why this is bad:

Docker exec inspect includes fields such as `CanRemove`, `OpenStdin`, `OpenStdout`, and `OpenStderr`. dd omits them, breaking clients that inspect exec stream capabilities or removal state.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-exec-health-wait-target cargo test -p dd-daemon audit_exec_inspect_reports_full_docker_state_shape -- --nocapture
```

Result: failed first on missing `CanRemove`.

## Health Starting Transition Is Asynchronous

Priority: P2
Impact: inspect immediately after start can miss expected health starting state
Confidence: High

Verification status: Source-proven in isolated worktree `/Users/x/dd/dd-audit-daemon-exec-health-wait`.

Evidence:

- Start marks the container running before health state is installed: `dd-daemon/src/containers/lifecycle/run.rs:29`.
- Health monitor setup happens later in spawn/monitor code: `dd-daemon/src/runtime/spawn/live.rs:193`, `dd-daemon/src/runtime/health.rs:80`.

Why this is bad:

For containers with a healthcheck, the health object should be visible as `starting` as part of the start transition. dd has a timing gap where inspect can report running with no health state yet, which causes pollers to miss the initial health lifecycle.

## Wait Condition Removed Returns Before Removal

Priority: P1
Impact: Docker wait clients can observe removal before the object is gone
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-wait-events-health-20260710`.

Evidence:

- Container wait does not extract or enforce the `condition` query: `dd-daemon/src/containers/lifecycle/manage.rs:43`.
- The test support route exercises the same endpoint behavior: `dd-daemon/src/test_support/containers.rs:50`.

Why this is bad:

Docker wait supports conditions such as `not-running`, `next-exit`, and `removed`. dd treats `condition=removed` like a generic exited wait and returns while the container still exists, so cleanup orchestration can race with later inspect/remove calls.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-wait-events-health-target cargo test -p dd-daemon --bin dd-daemon wait_condition_removed_does_not_complete_while_container_exists -- --nocapture
```

Result: `wait?condition=removed` returned immediately while the exited container still existed.

## Exec Lifecycle Events Are Missing

Priority: P1
Impact: Docker event consumers miss exec create/start/die transitions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-wait-events-health-20260710`.

Evidence:

- Exec create records exec state without publishing an event: `dd-daemon/src/containers/exec/create.rs:21`.
- Exec reaper records exit code without an exec die event: `dd-daemon/src/runtime/spawn/live.rs:276`.
- Test support confirmed no event was emitted: `dd-daemon/src/test_support/exec.rs:55`.

Why this is bad:

Docker emits container events for `exec_create`, `exec_start`, and `exec_die`. dd omits them, so event-driven clients cannot track exec lifecycle or correlate failures.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-wait-events-health-target cargo test -p dd-daemon --bin dd-daemon exec_create_emits_container_exec_create_event -- --nocapture
```

Result: timed out waiting for an `exec_create` event.

## `event=health_status` Filter Misses Health Transitions

Priority: P2
Impact: documented health event filters return empty streams
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-wait-events-health-20260710`.

Evidence:

- Event filters compare the requested event string exactly against `Action`: `dd-daemon/src/events.rs:117`.
- Health transitions emit actions such as `health_status: unhealthy`: `dd-daemon/src/runtime/health.rs:92`.
- Test support confirmed the filtered stream is empty: `dd-daemon/src/test_support/events.rs:32`.

Why this is bad:

Docker clients filter health transitions with `event=health_status`. dd emits action strings with the status suffix and then exact-matches the whole action, so health transition events are hidden from filtered consumers.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-wait-events-health-target cargo test -p dd-daemon --bin dd-daemon events_filter_health_status_matches_health_transition_actions -- --nocapture
```

Result: `event=health_status` produced an empty stream for `health_status: unhealthy`.

## Lifecycle Mutations Can Succeed Without Durable State

Priority: P1
Impact: successful lifecycle changes can disappear after daemon restart
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-fs-lifecycle-20260710`.

Evidence:

- State persistence is best-effort and returns no failure to callers: `dd-daemon/src/util/state.rs:4`.
- Network create mutates in-memory state before returning success: `dd-daemon/src/networks/handlers.rs:47`.

Why this is bad:

If `DD_STATE` cannot be written, a successful API response should either fail the request or roll back the in-memory mutation. dd can return `201 Created` for a network that will vanish on daemon restart.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-fs-lifecycle-20260710-target cargo test -p dd-daemon audit_network_create_fails_when_state_cannot_be_persisted -- --nocapture
```

Result: network create returned `201 Created` with an invalid state path.

## Volume Create/Delete/Prune Report Success While Storage Is Wrong

Priority: P1
Impact: volume state and backing directories silently diverge
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-fs-lifecycle-20260710`.

Evidence:

- Volume create ignores backing directory creation failure: `dd-daemon/src/volumes.rs:83`.
- Volume delete and prune drop state before best-effort directory removal: `dd-daemon/src/volumes.rs:130`, `dd-daemon/src/volumes.rs:159`.

Why this is bad:

Creating a volume without storage should fail, and deleting/pruning should remove the persisted mountpoint or preserve retryable state. dd reports success while storage is absent, stale, or left behind after `DD_VOLUMES` changes.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-fs-lifecycle-20260710-target cargo test -p dd-daemon audit_volume_ -- --nocapture
```

Result: create returned `201`; delete returned `204`; prune reported deleted volumes while old persisted mountpoints still existed.

## Container Rm/Prune Drop State When Writable-Layer Cleanup Fails

Priority: P2
Impact: failed cleanup leaves orphan writable layers with no retryable container state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-fs-lifecycle-20260710`.

Evidence:

- Container layer discard ignores `remove_dir_all` failure: `dd-daemon/src/containers/lifecycle/manage.rs:129`.
- Admin and diff helpers depend on writable-layer state: `dd-daemon/src/containers/inspect/admin.rs:15`, `dd-daemon/src/containers/inspect/diff.rs:10`.

Why this is bad:

If writable-layer deletion fails, `docker rm` or prune should return an error or keep state for retry. dd removes state and reports success, orphaning filesystem data that is no longer visible to daemon APIs.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-fs-lifecycle-20260710-target cargo test -p dd-daemon audit_container_ -- --nocapture
```

Result: `rm` returned `204`; prune reported `["prunefail"]`; the layer parent still existed.

## Image Rmi Reports Deletion When Backing Store Removal Fails

Priority: P2
Impact: image state can be dropped while the store entry remains on disk
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-fs-lifecycle-20260710`.

Evidence:

- Image delete removes the image from memory before removing backing storage: `dd-daemon/src/images/tags.rs:87`.
- Image directory removal ignores deletion errors and returns no status: `dd-images/src/image/archive/mod.rs:75`.

Why this is bad:

Failed image store removal should fail the API call or preserve image state for retry. dd can report `Deleted` while leaving an untracked store entry on disk.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-fs-lifecycle-20260710-target cargo test -p dd-daemon audit_image_rmi_keeps_state_when_store_removal_fails -- --nocapture
```

Result: `rmi` returned `200 OK` when backing store removal failed.

## Event Filters Broaden To Match-All For Supported Keys

Priority: P1
Impact: filtered event consumers process unrelated daemon objects
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-events-api-apiworker-20260710`.

Evidence:

- Event filters store only `type`, `event`/`action`, `container`, and `image`: `dd-daemon/src/events.rs:93`.
- Matching checks only those stored fields: `dd-daemon/src/events.rs:118`.
- Container labels are persisted but not emitted in create event attributes: `dd-daemon/src/containers/lifecycle/create/mod.rs:287`, `dd-daemon/src/containers/lifecycle/create/mod.rs:354`.
- Volume labels are stored but not emitted: `dd-daemon/src/volumes.rs:82`.

Why this is bad:

Docker supports filters such as `label`, `network`, `scope`, and `volume`. dd ignores those keys, so nonmatching filters leak unrelated lifecycle events instead of narrowing the stream.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-events-api-apiworker-20260710-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: `label=...`, `network=frontend`, `volume=cache`, and `scope=swarm` filters all leaked unrelated events.

## Malformed Filters JSON Becomes An Unfiltered Stream

Priority: P1
Impact: client filter encoding bugs subscribe to every daemon event
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-events-api-apiworker-20260710`.

Evidence:

- Bad filter JSON returns `Filters::default()`: `dd-daemon/src/events.rs:103`.
- The handler still returns `200 OK`: `dd-daemon/src/events.rs:229`.

Why this is bad:

Malformed JSON in the `filters` query should be a bad-parameter response. dd broadens it to match-all, so clients or proxies with encoding bugs can act on unrelated events.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-events-api-apiworker-20260710-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: malformed `{"type":["container"` became an unfiltered event stream.

## Non-Epoch Until Values Turn Bounded Events Into Unbounded Streams

Priority: P2
Impact: bounded event queries can hang indefinitely
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-events-api-apiworker-20260710`.

Evidence:

- Event time parsing accepts only integer seconds: `dd-daemon/src/events.rs:89`.
- Failed parse is treated as `None`: `dd-daemon/src/events.rs:189`.
- Stream closure only happens when `until` is `Some`: `dd-daemon/src/events.rs:204`.

Why this is bad:

Docker accepts Unix timestamps, date/RFC3339-style timestamps, and duration strings for event time bounds. dd ignores non-epoch `until` values, turning a bounded query into a live stream.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-events-api-apiworker-20260710-target cargo test -p dd-daemon audit_ -- --nocapture
```

Result: `2017-01-05T00:36:05Z` and `10m` both parsed as no `until` bound.

## Create With Missing Network Persists Partial Container State

Priority: P1
Impact: invalid container create can report success, emit events, and persist unusable state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-durability-20260710`.

Evidence:

- Container create ignores `join_network(...)` failure: `dd-daemon/src/containers/lifecycle/create/mod.rs:337`.
- The same path then emits a create event and inserts the container: `dd-daemon/src/containers/lifecycle/create/mod.rs:354`.

Why this is bad:

Creating a container attached to a missing network should fail atomically, usually with not found, and should not publish a container or event. dd returns `201 Created` and records partial state after network join failed.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-durability-20260710-target cargo test -p dd-daemon audit_create_unknown_network_is_atomic_and_does_not_publish_state -- --ignored --nocapture
```

Result: observed `201 Created`; expected `404 Not Found` and no recorded container.

## System Df Overcounts Containers For Sibling Tags

Priority: P2
Impact: image usage accounting attributes containers to the wrong tag
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-durability-20260710`.

Evidence:

- `system df` counts containers by repository only via `ref_repo(...)`: `dd-daemon/src/system.rs:109`.

Why this is bad:

Containers created from `repo/app:v1` should not count under sibling image `repo/app:v2`. dd reports `Containers=1` for the sibling tag, which can mislead cleanup and disk-usage tooling.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-durability-20260710-target cargo test -p dd-daemon audit_system_df_counts_containers_by_exact_image_tag_not_repository -- --ignored --nocapture
```

Result: `repo/app:v2` reported `Containers=1`; expected `0`.

## Plugin Inventory Endpoint Is Missing Despite Info Advertising Plugins

Priority: P3
Impact: clients see plugin capability hints but cannot query plugin inventory
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-daemon-api-durability-20260710`.

Evidence:

- Route table has `/info` and `/system/df`, but no `/plugins` route before fallback: `dd-daemon/src/routes.rs:21`.
- `/info` advertises plugin categories: `dd-daemon/src/system.rs:71`.

Why this is bad:

If dd advertises plugin categories in `/info`, Docker-compatible clients may query `/plugins`. With no installed plugins, an empty list is a better compatibility response than `404`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-daemon-api-durability-20260710-target cargo test -p dd-daemon audit_plugins_list_endpoint_returns_empty_list_not_404 -- --ignored --nocapture
```

Result: observed `404`; expected `200` with `[]`.

## Image Usage Active Count Counts Containers Not Images

Priority: P2
Impact: system df reports active image count as container count
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-system-endpoints-20260710`.

Evidence:

- Image usage active count is derived from container count: `dd-daemon/src/system.rs:187`.

Why this is bad:

With two running containers using one image, `ImageUsage.ActiveCount` should describe the one active image, not the two containers. dd reports `2`, inflating usage accounting.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-system-endpoints-20260710-target cargo test -p dd-daemon system_df -- --nocapture
```

Result: with two running containers using one image, `ImageUsage.ActiveCount = 2`; expected `1`.

## Volume Usage Never Reports Live References

Priority: P2
Impact: mounted volumes appear unused in system df output
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-system-endpoints-20260710`.

Evidence:

- Volume usage active count and ref count are hardcoded or not tied to container mounts: `dd-daemon/src/system.rs:156`, `dd-daemon/src/system.rs:201`.

Why this is bad:

A volume mounted by a container should report a live reference. dd reports `VolumeUsage.ActiveCount = 0` and `UsageData.RefCount = -1`, so cleanup tools can misclassify in-use volumes as unused.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-system-endpoints-20260710-target cargo test -p dd-daemon system_df -- --nocapture
```

Result: mounted volume reported active/ref counts `0` and `-1`; expected `1`.

## Build-Cache Totals Can Be Nonzero With Empty Item Lists

Priority: P2
Impact: system df reports contradictory build-cache accounting
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-system-endpoints-20260710`.

Evidence:

- Build-cache totals and item lists are computed separately: `dd-daemon/src/system.rs:209`, `dd-daemon/src/system.rs:212`, `dd-daemon/src/system.rs:217`.

Why this is bad:

If build-cache total count is nonzero, the endpoint should list the corresponding items or avoid advertising item-level counts. dd can report `BuildCacheUsage.TotalCount = 1` while both `BuildCacheUsage.Items` and top-level `BuildCache` are empty.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-system-endpoints-20260710-target cargo test -p dd-daemon system_df -- --nocapture
```

Result: one `~/.dd/pcache/*.pcache` file produced total count `1` with empty item lists.

## Info Under-Reports Daemon Capacity

Priority: P1
Impact: clients and schedulers see one CPU and zero memory
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-info-version-20260710`.

Evidence:

- `/info` hardcodes `NCPU: 1`: `dd-daemon/src/system.rs:60`.
- `/info` hardcodes `MemTotal: 0`: `dd-daemon/src/system.rs:61`.

Why this is bad:

Docker-compatible `/info` reports logical CPUs usable by the daemon and total physical memory in bytes. dd reports `NCPU=1` and `MemTotal=0` on a host where the proof test observed `18` CPUs and nonzero memory, so clients can under-size workloads or reject capacity assumptions.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-info-version-20260710-target cargo test -p dd-daemon system_info -- --ignored --nocapture
```

Result: `NCPU=1 MemTotal=0`, expected host-derived CPU count and nonzero memory.

## Info Default Runtime Is Not Declared

Priority: P1
Impact: runtime capability data is internally inconsistent
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-info-version-20260710`.

Evidence:

- `/info` sets `DefaultRuntime: "dd-jit"`: `dd-daemon/src/system.rs:66`.
- The system DTO has no `Runtimes` field at all: `dd-daemon/src/api/system.rs:10`.

Why this is bad:

Docker clients expect the default runtime to be declared in the runtimes map. dd advertises `dd-jit` as default but omits `Runtimes`, so runtime validation and capability discovery see a broken shape.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-info-version-20260710-target cargo test -p dd-daemon system_info -- --ignored --nocapture
```

Result: `DefaultRuntime="dd-jit"` and `Runtimes=null`.

## Daemon Version Endpoints And Server Header Are Stale

Priority: P2
Impact: clients and diagnostics receive stale daemon identity
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-info-version-20260710`.

Evidence:

- Daemon crate version is `0.4.0`: `dd-daemon/Cargo.toml:3`.
- `/version` returns `0.1.0-dd`: `dd-daemon/src/system.rs:9`, `dd-daemon/src/system.rs:22`.
- `/info` returns `ServerVersion: 0.1.0-dd`: `dd-daemon/src/system.rs:63`.
- Response `Server` header returns `dd-daemon/0.1.0`: `dd-daemon/src/http.rs:89`.

Why this is bad:

Version endpoints and headers should track the built daemon version or build metadata consistently. Stale identity breaks compatibility gates and support diagnostics.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-info-version-20260710-target cargo test -p dd-daemon audit_version_info_and_server_header_track_crate_version -- --ignored --nocapture
```

Result: `/version`, `/info`, and `Server` header reported `0.1.0*`; expected `0.4.0`.

## Create Accepts Image Records Whose Rootfs Is Missing

Priority: P1
Impact: containers can be recorded with nonexistent backing rootfs paths
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-create-start-atomicity-20260710`.

Evidence:

- Container create resolves the image from metadata: `dd-daemon/src/containers/lifecycle/create/mod.rs:54`.
- The selected image rootfs is recorded without validating that it exists: `dd-daemon/src/containers/lifecycle/create/mod.rs:252`.

Why this is bad:

Creating a container from an image whose rootfs path has disappeared should fail and emit no lifecycle event. dd returns `201`, emits `container/create`, and records a container pointing at `gone-rootfs`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-create-start-atomicity-20260710-target cargo test -p dd-daemon audit_create_rejects_image_with_missing_rootfs -- --ignored --nocapture
```

Result: returned `201`, emitted `container/create`, and recorded the missing rootfs path.

## Anonymous Volume Materialization Failures Are Ignored

Priority: P1
Impact: create records containers and volumes whose mountpoints do not exist
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-create-start-atomicity-20260710`.

Evidence:

- Anonymous volume creation ignores `create_dir_all` and copy-up errors: `dd-daemon/src/containers/lifecycle/create/volumes.rs:21`, `dd-daemon/src/containers/lifecycle/create/volumes.rs:52`.
- Container create records the volume/container after that path: `dd-daemon/src/containers/lifecycle/create/mod.rs:226`.

Why this is bad:

If anonymous volume storage cannot be created or seeded, container create should fail or roll back side effects. dd returns `201`, emits `volume/create`, and records a volume whose mountpoint does not exist.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-create-start-atomicity-20260710-target cargo test -p dd-daemon audit_create_rejects_anonymous_volume_when_backing_dir_cannot_be_created -- --ignored --nocapture
```

Result: with the volumes root set to a file, create returned `201` and recorded one container plus one nonexistent volume mountpoint.

## Create Events Can Be Emitted Before Durable State Success

Priority: P2
Impact: event consumers can observe containers that were never durably saved
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-create-start-atomicity-20260710`.

Evidence:

- `container/create` is emitted before state persistence: `dd-daemon/src/containers/lifecycle/create/mod.rs:354`, `dd-daemon/src/containers/lifecycle/create/mod.rs:361`.
- State persistence logs failure without aborting the operation: `dd-daemon/src/util/state.rs:4`.

Why this is bad:

Create events should represent durable lifecycle state or the daemon should document non-durable behavior explicitly. dd can emit `container/create`, return `201`, and keep an in-memory container even when state save failed.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-create-start-atomicity-20260710-target cargo test -p dd-daemon audit_create_does_not_emit_container_event_before_durable_state -- --ignored --nocapture
```

Result: with `state_path` as a directory, save failed, but the handler returned `201`, emitted `container/create`, and kept one in-memory container.
