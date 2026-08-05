# Repository compatibility scenarios

Each direct child directory owns one `test.yaml` definition and its local source
and golden files. The repository testing application discovers those definitions
without a Rust registry or category wrapper.

Run the quick suite with:

```text
nix develop --command cargo run -p testing -- scenarios --class quick
```

List one scenario without materializing images:

```text
nix develop --command cargo run -p testing -- scenarios languages --list
```

## Ownership and preservation

Every direct child with `test.yaml` is an active repository end-to-end test and
owns its local source, input, golden, and oracle files. Rust tests of one
crate's public API live in that crate. The retired root Rust registries,
category wrappers, fixtures, and workflow modules were detached orchestration,
not scenario declarations, and have been removed after their behavior acquired
an executable owner.

The migration reconciled the legacy declarative inventory at `747c2b3d0`.
All 19 formerly old-only stable IDs retain the same image, action semantics,
targets, expected-failure metadata, timeout, and output oracle in folder-owned
definitions. The former database cleanup mapping remains in its category
oracle; language uniqueness and unusual expected-failure invariants have
focused testing-application unit tests. The 14 category-local `ORACLE.md` files
remain the authoritative per-case record of commands, readiness, entrypoints,
scheduler differences, and replacement owners.

There is no active root `registry/`, `main/`, `harness/`, `fixtures/`, or
`golden/` directory. Discovery, scheduling, execution, and reporting belong to
the `testing` application without duplicating declarations.

## Retired API-group ownership

| Removed group | Durable public-contract evidence |
|---|---|
| copy | `hl-daemon/tests/api/container_copy.rs`, `hl-container/tests/filesystem_coherence.rs`, and typed client stat/copy/export coverage |
| exec command | `hl-container/tests/process_contract.rs` and `hl-client/tests/execution.rs` |
| images | typed client image archive tests and daemon image archive/prune tests |
| container networking | daemon live name-based bridge traffic plus `hl-container/tests/networks.rs` endpoint, IP, and alias ownership |
| networks | container topology, daemon bridge/list/built-in, and typed client network tests |
| run flags | `hl-container/tests/run_options.rs`, process contracts, and daemon/client create, network, publication, resource, and removal tests |
| volumes | `hl-container/tests/volumes.rs`, daemon volume/system-disk tests, and typed client volume tests |
| observability | typed client and daemon inspect, list, logs, archive, publication, prune, top, stats, and option-validation tests |

Redis, netcat, ping, and shell choices in those groups were test vehicles, not
independent reusable behavior. Multi-package application scenarios instead
belong in discoverable direct child folders.

## Legacy action ownership

The retired opaque inventory contained 140 actions: 72 direct API actions and
68 host actions. Its ownership split was 67 package contracts and 73 repository
E2E contracts. The five daemon-backed `runflags` cases were `publish-p`, `rm`,
`user-name`, `network-bridge`, and `env-e`; despite the old `api` label they ran
through an embedded daemon and typed client. Expected values were embedded in
Rust assertions or registry checks rather than folder-owned golden files.

| Legacy prefix | Cases | Durable owner/evidence |
|---|---:|---|
| `cpcoherence` | 8 | `hl-container` filesystem coherence contract |
| `execcmd` | 7 | container execution and typed client contracts |
| `lifecycle` | 17 | container lifecycle policy and real-engine lifecycle acceptance |
| `netcontainer` | 5 | container network topology and daemon bridge traffic |
| `process` | 15 | `hl-container/tests/process_contract.rs` |
| `runflags` | 20 | container option contracts plus daemon/client E2E |
| `buildcmd` | 2 | repository build workflow gap tracked in the pipeline |
| `cpcmd` | 4 | folder-owned copy YAML and goldens |
| `dockernet` | 8 | daemon/client network contracts and repository E2E |
| `dockervol` | 17 | container/daemon/client volume contracts and repository E2E |
| `imagescmd` | 5 | typed client and daemon image contracts |
| `observe` | 16 | typed client and daemon observability contracts |
| `volumes` | 16 | folder-owned filesystem/volume YAML and goldens |

### Exact legacy action IDs

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

The four `cpcoherence` behaviors—new-file visibility, cached-positive
replacement, extracted-tree visibility, and extraction into a held directory—
run for both guest ISAs through `hl-container/tests/filesystem_coherence.rs`.
The retained C oracle audit covered `fdcache.c::{fsgen_bind,fsgen_flush,
hl_fdcache_generation_poll,hl_fdcache_resolution_bump,hl_fdcache_reset}`;
the pre-dispatch poll in `linux_abi/syscall/dispatch.c`; and generation-path
propagation in `core/launch.c`. Rust ownership is the fixed-width generation
file and publication in `generation.rs`, post-extraction publication in
`filesystem.rs`, attachment by the container filesystem service, propagation
through launch/exec/health, and forwarding by `hl-engine`. Publication occurs
after mutation; acquire observation invalidates caches before syscall dispatch.

The lifecycle move preserved all 17 IDs across focused policy tests and real
Linux acceptance for signals, pause, and health. Its retained C audit covered
`core/activation.c::{activation_start,activation_signal_relay,hl_activation_wait,
hl_activation_try_wait,hl_activation_kill,hl_activation_domain_processes,
hl_activation_domain_terminate,hl_activation_process_destroy}`;
`core/lifecycle.c::{hl_production_start_process,hl_production_finish_process}`;
POSIX host process opening; and Windows wait, terminate, and close. The
activation owns child/process-domain handles through teardown, validates the
control handshake, retries interruptible waits, caches completion for repeated
observation, and kills/reaps unfinished descendants. Rust owns durable records,
generation identity, restart policy, pause, health, rename, and removal in the
container service.

| Legacy lifecycle IDs | Owning contract test |
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

The 15 process rows remain real guest tests because PID 1, uid/gid, cwd,
environment, signals, streams, and shared filesystem state cannot be proven by
a fake runtime. The retained C audit covered AArch64 and x86 image loading and
initial stacks; uid/gid and transactional `execve` in `syscall/proc.c`; chdir
and fchdir in `syscall/fs.c`; hostname in `syscall/misc.c`; and activation and
lifecycle result publication. `execve` validates before commit, stops siblings,
closes CLOEXEC descriptors, resets mappings/code caches/caught handlers, and
loads the replacement without returning to the old image. Rust composition is
owned by typed process models, launch/exec transactions, ordered stream
journals, and the `hl-engine` architecture/host boundary.

These focused moves do not claim their broader runtime domains complete; the
category oracle and current executable tests define the accepted scope.
