# API and host action ownership

This audit classifies the 140 legacy scenario contracts whose action was opaque
to the YAML runner. It is based on the former generated contract snapshot, the
legacy group and registry implementations, and the public package boundaries.
The historical snapshot has 72 `api` actions and 68 `host` actions; deleted
artifacts remain available through repository history.

## Ownership matrix

| Legacy prefix | Actions | Cases | Owner after migration | Legacy implementation | Expected evidence |
|---|---:|---:|---|---|---|
| `cpcoherence` | API | 8 | `hl-container` public filesystem contract | `groups/coherence.rs`, duplicated in `groups/copy.rs` | Rust assertions over exit status and captured output |
| `execcmd` | API | 7 | `hl-container` execution contract | `groups/execcmd.rs` | Rust assertions over execution state, streams, user, workdir, and stdin |
| `lifecycle` | API | 17 | `hl-container` lifecycle contract | `groups/lifecycle.rs` | Rust assertions over state, signals, restart, health, rename, wait, and removal |
| `netcontainer` | API | 5 | `hl-container` network/runtime contract | `groups/netcontainer.rs` | Rust assertions over endpoints, reachability, and isolation |
| `process` | API | 15 | `hl-container` process contract | `groups/process.rs` | Rust assertions over process configuration, exit, signals, streams, and exec |
| `runflags` direct | API | 15 | `hl-container` specification/runtime contracts | `groups/runflags.rs` | Rust assertions over the public headless API |
| `runflags` daemon | API | 5 | repository Docker E2E | `groups/runflags_docker.rs` | folder-owned YAML and golden output through daemon/client |
| `buildcmd` | host | 2 | repository Docker CLI E2E | `registry/build.rs`, `workflows/build.rs` | folder-owned YAML, source context, and golden output |
| `cpcmd` | host | 4 | repository Docker CLI E2E | `registry/copy.rs`, `groups/copy.rs` | folder-owned YAML, source archive/input, and golden output |
| `dockernet` | host | 8 | repository Docker CLI E2E | `registry/dockernet.rs`, `groups/network.rs` | folder-owned YAML and golden output |
| `dockervol` | host | 17 | repository Docker CLI E2E | `registry/dockervol.rs`, `groups/volume/` | folder-owned YAML, input trees, and golden output |
| `imagescmd` | host | 5 | repository Docker CLI E2E | `registry/images.rs`, `groups/imagescmd.rs` | folder-owned YAML, image archive/source, and golden output |
| `observe` | host | 16 | repository Docker CLI E2E | `registry/observe.rs`, `groups/observe.rs` | folder-owned YAML and golden output |
| `volumes` | host | 16 | repository Docker CLI E2E | `registry/volume.rs`, `groups/volume/` | folder-owned YAML, input trees, and golden output |
| **Total** |  | **140** | **67 package / 73 E2E** |  |  |

The five daemon-owned `runflags` IDs are `publish-p`, `rm`, `user-name`,
`network-bridge`, and `env-e`. Although their registry action says `api`, the
legacy dispatcher executes them through an embedded daemon and `hl-client`.

There are no folder-owned golden files for these 140 contracts. Expected values
are embedded in Rust strings or registry `contains` checks; the only shared
artifact was the former generated contract snapshot. The E2E moves
must extract those values into each category's `golden/` directory. Package moves
should keep typed Rust assertions beside the public contract instead.

### Exact legacy IDs

Each suffix below is joined to the prefix with `/`:

| Prefix | IDs |
|---|---|
| `cpcoherence` | `cp-dir-tree-live-poll[.amd]`, `cp-into-held-open-dir[.amd]`, `cp-new-file-live-poll[.amd]`, `cp-overwrite-cached-positive[.amd]` |
| `execcmd` | `basic`, `detached-d`, `env-e`, `exit-code`, `stdin-i`, `user-u`, `workdir-w` |
| `lifecycle` | `create-start`, `healthcheck-healthy`, `healthcheck-unhealthy`, `kill-signal`, `pause-unpause`, `rename`, `restart`, `restart-on-failure-count`, `rm`, `rm-force`, `rm-multi`, `rm-multi-force`, `stop`, `stop-signal-inspect`, `stop-signal-quit`, `unless-stopped-manual`, `wait` |
| `netcontainer` | `isolation-off-network`, `nc-echo-by-name`, `ping-by-name`, `redis-by-ip`, `redis-by-name` |
| `process` | `env-multiple`, `env-passthrough`, `exec-env`, `exec-into-running`, `exec-sees-shared-fs`, `exit-nonzero`, `exit-rc-check`, `exit-zero`, `hostname-flag`, `pid1-is-init`, `sigterm-clean-stop`, `stdout-stderr-split`, `uid-root`, `workdir`, `workdir-created` |
| `runflags` | `bind-mount-v`, `cpus-accepted`, `detached-d`, `entrypoint`, `env-e`, `exit-code`, `memory-accepted`, `memory-cgroup-honored`, `name`, `network-bridge`, `network-none`, `publish-p`, `publish-p-explicit`, `restart-on-failure`, `rm`, `stdin-i`, `tty-t`, `user-name`, `user-uidgid`, `workdir-w` |
| `buildcmd` | `full`, `simple` |
| `cpcmd` | `container-to-host-dir`, `container-to-host-file`, `host-to-container-dir`, `host-to-container-file` |
| `dockernet` | `connect`, `create-ls`, `create-multi-alias`, `host-mode`, `inspect`, `reach-by-name`, `reach-by-name-late`, `rm` |
| `dockervol` | `anon-volume`, `bind-nonrecursive-reject`, `bind-private-recursive-ro`, `bind-shared-reject`, `create-ls`, `inspect`, `local-bind`, `local-bind-inspect`, `local-filesystem-reject`, `mount-tmpfs`, `mount-volume-inuse`, `persist-across-runs`, `rm`, `subpath`, `subpath-missing`, `subpath-symlink-escape`, `tmpfs-fresh` |
| `imagescmd` | `history`, `inspect`, `list`, `rmi`, `tag` |
| `observe` | `container-prune-filter`, `inspect-cmd`, `inspect-config-env`, `inspect-mounts`, `inspect-network-ip`, `inspect-state`, `logs`, `logs-follow`, `logs-tail`, `port`, `ps-all-exited`, `ps-ports`, `ps-running`, `stats-oneshot`, `system-prune-filter-reject`, `top` |
| `volumes` | `cmd-append-redirect`, `cmd-cat-grep-wc`, `cmd-chmod-perms`, `cmd-cp-mv-rm`, `cmd-mkdir-touch-find`, `cmd-sed-inplace`, `cmd-sort-head-tail`, `cmd-wc-bytes`, `delete-propagates`, `host-seen-in-container`, `nested-dotdot-crosses-boundary`, `persist-across-runs`, `readonly-rejects-write`, `subdir-mount`, `two-mounts`, `write-seen-on-host` |

## First ownership move

The eight `cpcoherence` inventory rows represent four behaviors on both guest
ISAs:

| Behavior | ARM64 ID | AMD64 ID |
|---|---|---|
| new file observed by a running process | `cp-new-file-live-poll` | `cp-new-file-live-poll.amd` |
| cached positive entry observes overwrite | `cp-overwrite-cached-positive` | `cp-overwrite-cached-positive.amd` |
| extracted directory tree becomes visible | `cp-dir-tree-live-poll` | `cp-dir-tree-live-poll.amd` |
| process holding the destination directory observes extraction | `cp-into-held-open-dir` | `cp-into-held-open-dir.amd` |

These exercise `Containers::filesystem(...).extract(...)`, not a Docker client or
CLI contract. They therefore belong to the `hl-container` public-contract test.
The package test selects the guest ISA with `HL_SCENARIO_TARGET` and consumes the
pinned `HL_ALPINE_ARCHIVE`, so the same four test functions form all eight
case/ISA rows without maintaining `.amd` duplicate IDs.

After this move, the opaque legacy inventory is 132 cases: 64 `api` plus 68
`host`. Its ownership split is 59 package contracts and 73 repository E2E cases.

### Retained C oracle audit

The read-only C implementation studied for this move was:

- `../engine/src/linux_abi/fdcache.c`: `fsgen_bind`, `fsgen_flush`,
  `hl_fdcache_generation_poll`, `hl_fdcache_resolution_bump`, and
  `hl_fdcache_reset`;
- `../engine/src/linux_abi/syscall/dispatch.c`: the pre-dispatch generation poll;
- `../engine/src/core/launch.c`: propagation of the filesystem-generation path.

The daemon owns one fixed-width generation file for each container. Every run,
exec, and health engine maps that same file; each engine owns its last-seen value
and its descriptor/path caches. An external writer completes the filesystem
mutation before publishing the generation. The engine's next syscall observes
the changed word, performs an acquire load, drops all affected caches, and only
then dispatches the syscall. Guest namespace mutations bump the shared epoch as
well. The cache mutex protects threaded mutation; mappings are released on
rebinding, and fork descendants inherit the shared state. The generation-file
mapping is host-service based; only the separate fork-local fallback has a
Windows-specific branch.

The Rust ownership map is:

- `generation.rs::Generation::{open,bump}` owns the fixed-width file and
  publication;
- `filesystem.rs::Filesystem::extract_with` publishes only after successful
  extraction;
- `service/container/filesystem.rs` attaches the generation to the public
  filesystem surface;
- `service/container/{launch,exec,health}.rs` gives every engine the same path;
- `hl-engine` launcher configuration forwards the path to the native boundary.

The four moved tests cover creation after a negative lookup, replacement after a
positive lookup, tree creation, and a held-directory-relative lookup. They do not
claim the broader filesystem cache domain complete.

## Lifecycle ownership move

The 17 `lifecycle` rows were opaque API dispatches into `groups/lifecycle.rs`.
They exercised the public `hl_container::Containers` lifecycle rather than a
daemon, client, or multi-package workflow, so their durable home is the owning
crate's public-contract tests. The old Alpine shell processes mixed engine
signal behavior with container policy and ran both declared targets from one
untyped Rust switch. Focused `FakeRuntime` tests now isolate the policy and run
in parallel without an image fixture.

| Legacy IDs | Owning contract test |
|---|---|
| `create-start`, `wait` | `lifecycle_has_single_owner_and_supports_many_waiters` |
| `kill-signal` | unit `raw_signal_while_running_does_not_suppress_automatic_restart`; integration `hangup_reaches_the_guest_signal_handler` |
| `restart` | `manually_stopped_container_starts_a_new_generation` |
| `pause-unpause` | unit `pause_and_unpause_are_persisted_runtime_transitions`; integration `pause_stops_guest_progress_until_unpause` |
| `rm`, `rm-force`, `rename`, `stop` | `rename_wait_removed_stop_and_force_remove_follow_owned_lifecycle` |
| `rm-multi`, `rm-multi-force` | `independent_normal_and_force_removals_leave_no_records` |
| `stop-signal-inspect` | unit `configured_stop_signal_is_durable_and_used_for_graceful_stop` |
| `stop-signal-quit` | integration `configured_quit_reaches_the_guest_signal_handler` |
| `restart-on-failure-count` | `on_failure_restarts_exactly_to_limit_and_wait_spans_backoff` |
| `unless-stopped-manual` | `manual_stop_suppresses_unless_stopped_restart` |
| `healthcheck-healthy` | unit `health_monitor_persists_success_and_inherits_process_context`; integration `health_probes_reach_healthy_and_unhealthy_states` |
| `healthcheck-unhealthy` | unit `health_grace_threshold_timeout_and_pause_are_generation_safe`; integration `health_probes_reach_healthy_and_unhealthy_states` |

The multi-remove rows did not specify a distinct batch API: the legacy loop made
three independent `remove` or `remove_force` calls. The package test preserves
that independence while asserting that no records remain.

`hl-container/tests/lifecycle_contract.rs` retains the Linux acceptance layer
that a fake runtime cannot prove. Its four ignored tests consume the pinned
Alpine archive and select either guest ISA through `HL_SCENARIO_TARGET`; they
observe HUP and configured QUIT trap output, prove that guest progress stops
while paused and resumes afterward, and execute both successful and failing
health probes to their durable states. The unit tests remain the fast policy
layer, while these integration tests preserve real engine coverage.

After this move, the opaque legacy inventory is 115 cases: 47 `api` plus 68
`host`. Its ownership split is 42 remaining package contracts and 73 repository
E2E cases.

### Retained C oracle audit

The read-only lifecycle implementation studied was:

- `../engine/src/core/activation.c`: `activation_start`,
  `activation_signal_relay`, `hl_activation_wait`,
  `hl_activation_try_wait`, `hl_activation_kill`,
  `hl_activation_domain_processes`, `hl_activation_domain_terminate`, and
  `hl_activation_process_destroy`;
- `../engine/src/core/lifecycle.c`: `hl_production_start_process` and
  `hl_production_finish_process`;
- `../engine/src/host/linux/process.c` and
  `../engine/src/host/macos/process.c`: `hl_host_process_open`;
- `../engine/src/host/windows/process.c`:
  `hl_windows_process_wait`, `hl_windows_process_terminate`, and
  `hl_windows_process_close`.

The activation process owns the child PID, control descriptor, nonce, launch
domain, terminal master, cached terminal result, and platform handles until
destroy. POSIX launch creates a dedicated process group, completes a nonce-bound
control handshake, and tears down a failed launch by killing and waiting. Wait
retries interruptible host waits, validates the child reply before publishing a
guest exit, caches the result for repeated observation, and closes the control
descriptor exactly once. Force termination first targets the POSIX process
group and then repeatedly drains launch-domain membership so `setsid`, exec, and
reparenting cannot escape. Windows uses job objects for the equivalent tree
lifetime, retains completion for concurrent waiters under the host handle-table
lock, and refuses close while a waiter still borrows the handle.

Signal delivery is deliberately platform-specific. The POSIX activation relay
converts host signal numbers to Linux values, writes them through a self-pipe,
and serializes engine request publication with `activation_engine_lock`; forked
children reset inherited relay handlers. Windows can faithfully express only
interrupt and kill and returns not-supported for other catchable signals rather
than misreporting delivery. Engine result storage is shared and published with
acquire/release ordering; activation teardown kills and reaps an unfinished
child before releasing owned storage.

The C domain ends at launch, signal, wait, process-tree termination, and decoded
exit status. Durable container records, generation identity, restart backoff and
manual suppression, configured stop policy, pause state, health monitoring,
rename uniqueness, and removal are Rust `hl-container` responsibilities. Their
owners are `service/container/control.rs`, `removal.rs`, `restart.rs`,
`health.rs`, and the container repository, exposed only through `Containers`.
The moved tests exercise that public boundary with a typed runtime port; they do
not claim that every host can deliver every Linux signal.

## Process ownership move

The 15 `process` rows invoked `hl_container::Containers` directly and depended
on a real Alpine root filesystem. They now live in
`hl-container/tests/process_contract.rs`, selected for ARM64 or AMD64 by
`HL_SCENARIO_TARGET` and supplied only by the pinned `HL_ALPINE_ARCHIVE`.
Unlike lifecycle policy, these checks cannot be replaced by `FakeRuntime`: PID
1, Linux uid, `chdir`, environment, signal-handler, stream, and shared guest
filesystem behavior must execute inside the guest.

| Package test | Legacy IDs |
|---|---|
| `launch_contracts` | `env-multiple`, `env-passthrough`, `exit-nonzero`, `exit-rc-check`, `exit-zero`, `hostname-flag`, `pid1-is-init`, `stdout-stderr-split`, `uid-root`, `workdir`, `workdir-created` |
| `sigterm_stop` | `sigterm-clean-stop` |
| `exec_contracts` | `exec-env`, `exec-into-running`, `exec-sees-shared-fs` |

Each test owns its state directory and unpacked image. Container and exec names
are local to that state, cleanup is explicit, output comparisons are byte exact,
and the three tests can run independently. Missing fixture input is represented
as an ignored external-fixture test rather than a false pass.

After this move, the opaque legacy inventory is 100 cases: 32 `api` plus 68
`host`. Its ownership split is 27 remaining package contracts and 73 repository
E2E cases.

### Retained C oracle audit

The read-only process implementation studied was:

- `../engine/src/core/target/aarch64.c`: `container_init`, `build_stack`,
  `run_loaded`, and `hl_run_linux_guest`;
- `../engine/src/linux_abi/x86.c`: `build_stack` and the x86 image loader/run
  path;
- `../engine/src/linux_abi/syscall/proc.c`: uid/gid operations and the complete
  `execve` case 221 transaction;
- `../engine/src/linux_abi/syscall/fs.c`: `chdir` case 49 and `fchdir` case 50;
- `../engine/src/linux_abi/syscall/misc.c`: hostname operation case 161;
- `../engine/src/core/activation.c`: activation start, signal relay, wait, kill,
  and process teardown;
- `../engine/src/core/lifecycle.c`: production process start and decoded result
  publication.

The retained engine owns the guest address space, initial PID identity, loaded
ELF and stack, argv/environment bytes, guest cwd, uid/gid state, hostname, file
descriptors, signal state, and final Linux exit result for one activation.
Initial launch seeds root identity unless typed uid/gid override it, constructs
architecture-specific stacks from the same typed inputs, and keeps stdout and
stderr as distinct inherited descriptors. Guest `chdir` resolves through the
confined namespace and records the canonical guest cwd. `execve` is an
in-process replacement transaction: it validates the complete target before
commit, stops sibling threads, closes CLOEXEC descriptors, resets mappings,
code caches and caught signal handlers, forwards the exact requested
environment, loads the new image, and redirects execution without returning to
the old image. Signal relay and wait preserve guest signal/exit distinction;
unfinished activation teardown kills and reaps the owned process tree.

Architecture branches differ in ELF loading, initial stack layout, register
entry, and translated code-cache reset. Host branches differ in process launch,
watching, and termination: Linux uses pidfd when available, macOS uses kqueue,
POSIX activation uses process groups plus the durable launch-domain registry,
and Windows uses process/job handles while refusing signals it cannot deliver
faithfully. Container exec retains the same rootfs, identity mounts, network
namespace, filesystem-generation path, and process domain while assigning the
exec its own runtime handle, journal, wait state, and teardown.

The Rust ownership map is `model/process.rs` and `process_spec.rs` for typed
inputs and validation; `service/container/launch.rs` for the initial launch
transaction; `service/container/exec.rs` for exec identity, shared runtime
context, and independent completion; `service/io.rs` for ordered stream
journals; and `hl-engine` for the architecture/host execution boundary. The
moved tests cover observable composition at the public `Containers` and
`Executions` APIs and do not duplicate syscall implementation in the container
crate.

## Runner gaps for the 73 E2E cases

`src/apps/testing/src/scenario/definition.rs` already parses ordered `host` and
`api` actions. Execution deliberately refuses them in
`src/apps/testing/src/scenario/execution.rs` because these adapters do not yet
exist:

- a bounded host workspace and shell executor;
- lifecycle ownership for an isolated daemon and socket;
- Docker CLI/client selection and typed environment substitution;
- named resources shared across ordered actions, with unconditional cleanup;
- bounded stdout/stderr capture and durable per-action diagnostics;
- readiness and multi-action execution (the executor currently requires exactly
  one action and rejects readiness).

Adding a generic `api` string switch would preserve the opaque legacy design.
Package-owned behavior should move to crate tests; only daemon/client/CLI
workflows should gain typed YAML actions.
