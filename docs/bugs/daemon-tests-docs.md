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

