# Process supervision oracle audit

This audit covers the host subprocess boundary used to isolate one runtime
compatibility row. The retained C engine is read-only and is evidence for
lifecycle mechanics; its activation protocol is not copied into the generic
`hl-process` package.

## Retained implementation studied

- `../engine/tools/matrix_runner.c`
  - POSIX `run_guest`, `terminate`, and interrupt handling
  - Windows `run_guest`, `windows_child_close`, and
    `windows_child_terminate`
- `../engine/tools/linux_matrix.c`
  - POSIX and Windows `run_case` arms
- `../engine/tools/remote_supervisor.c`
  - `main`, `terminate_group`, and `terminate_remaining_group`
- `../engine/tests/integration/remote_supervisor.c`
  - capture, cancellation, and outliving-descendant contract probes
- `../engine/src/core/activation.c`
  - `activation_start` (POSIX and Windows arms)
  - `wait_child`, `wait_child_process`, `hl_activation_wait`, and
    `hl_activation_try_wait`
  - `hl_activation_kill`, `hl_activation_domain_terminate`, and
    `hl_activation_process_destroy`
  - `activation_handshake` and the failure exits around process creation
- `../engine/src/core/lifecycle.c`
  - `hl_production_entry` and the Windows launch reconstruction documented at
    the production-entry boundary

## Ownership and lifetime

The matrix runner owns one direct child/process group per case, two bounded
capture buffers, capture descriptors, the deadline, and the obligation to reap.
The remote supervisor repeats that ownership around a forwarded engine and
emits a heartbeat; loss of its bridge or `SIGHUP`, `SIGINT`, or `SIGTERM`
terminates and reaps the group. Its normal-exit path also terminates remaining
group members before returning, so descendants cannot keep capture writers
open. No lock protects this state: one runner thread owns it, while signal
handlers communicate through `sig_atomic_t` flags and an active group identity.
`linux_matrix.c::run_case` is the smaller local analogue: it owns one child,
deadline, capture, and expected result at a time. Its POSIX arm creates a group
in both parent and child to close the scheduling race, polls `waitpid`, and
kills/reaps on timeout or stall. Its Windows arm uses the same suspended-create,
job-assignment, resume, terminate, wait, and close sequence as the full runner.

The POSIX activation handle owns the direct child identity, its control
descriptor, a nonce-backed launch domain, and the obligation to kill and reap
an unfinished child during destruction. A non-terminal launch starts in a new
process group with `POSIX_SPAWN_SETPGROUP`; terminal launch uses `fork`,
`setsid`, and a controlling terminal. Failure after spawn kills the negative
process-group identity and waits through `waitpid`, retrying `EINTR`.

The Windows matrix-runner arm owns a process handle, primary-thread handle,
capture handles, and one Job Object. It creates the child suspended, assigns it
to a `KILL_ON_JOB_CLOSE` job, and only then resumes it. The zero-instruction
ordering is both the descendant-containment boundary and the stall detector's
CPU-accounting boundary.

The Windows activation handle owns the process handle and two Job Object
handles. `CreateProcessW` starts suspended with an explicit inherited-handle
list. Job assignment happens before `ResumeThread`, closing the race in which a
child could spawn an unowned descendant. `TerminateJobObject` supplies group
kill semantics, and all process, thread, inheritance, pipe, and job handles are
closed on every failure path. There is no shared mutable supervisor state and
therefore no supervisor lock; kernel process-group or Job Object membership is
the concurrency authority.

The activation protocol waits for a nonce-matched child reply before exposing a
live handle and records a final result once. `hl_activation_process_destroy`
kills and waits when the result was not already recorded. POSIX domain records
add identity across `setsid`, exec, and reparenting; the generic test-process
supervisor intentionally owns trusted workers that must not escape their host
process group. Windows Job Object membership remains authoritative across
ordinary descendant group changes.

## Ordering and failure semantics

The matrix runner limits capture to 1 MiB stdout and 64 KiB stderr and treats
timeout, stall, supervision failure, and normal exit as distinct outcomes. Its
POSIX read/wait loop tolerates `EINTR`, advances a monotonic deadline, drains
both streams until EOF, and never joins/finishes capture before the remaining
group is gone. The remote supervisor applies a 500 ms TERM grace followed by
KILL and repeats group-existence checks after reaping the leader.

The load-bearing order is create suspended or in a new group, constrain
inheritance, establish lifecycle-domain membership, run, terminate the complete
domain on cancellation/failure, close writers, and reap before releasing the
owner. POSIX wait retries `EINTR`; an absent group during repeated termination
is success. Windows uses a distinct forced-termination status in the engine,
while the testing supervisor reports its own typed timeout, cancellation, and
output-limit outcomes before decoding an ordinary exit.

## Rust mapping

| Retained C capability | Rust owner | State |
| --- | --- | --- |
| New POSIX process group before exec | `hl-process/src/unix.rs::run` | Implemented |
| Whole-group graceful then forced teardown | `hl-process/src/unix.rs::OwnedChild` | Implemented for trusted non-escaping workers |
| Direct-child reap and signal/exit distinction | `hl-process/src/unix.rs::OwnedChild` | Implemented |
| Suspended Windows creation | `hl-process/src/windows.rs::run` | Implemented |
| Explicit inherited-handle allowlist | `hl-process/src/windows.rs::Attributes` | Implemented |
| Job assignment before first instruction | `hl-process/src/windows.rs::run` | Implemented |
| Job kill plus kill-on-close RAII backstop | `hl-process/src/windows.rs::OwnedProcess` | Implemented |
| Bounded stdout and stderr ownership | `hl-process` platform `Drain` types | Implemented |
| One typed runtime row per subprocess | `apps/testing/src/runtime/execution/worker.rs` | Implemented |
| Typed engine launch settings | `HL_COMPAT_ENGINE_OPTIONS` consumed by the normal runtime inventory path | Implemented; the supervisor injects no engine setting |
| POSIX identity beyond `setsid` | retained activation launch-domain registry | Deliberately absent: test workers are trusted not to escape their group |

## Integrated engine gap: durable launch domains

The statement above is valid only for the repository-owned `hl-process` test
worker. It does not describe the integrated engine/container lifecycle. An
engine guest may legitimately call `setsid`, outlive its initial process, and be
reparented. The retained engine therefore uses a launch domain in addition to a
process group. The retired Rust executor carried only the opaque identity in
`hl-engine/src/domain.rs` and the launch wire. `ProcessSyscalls` can spawn, wait,
and signal a pid or process group, but cannot publish or enumerate membership,
read a process birth identity, terminate a domain, or remove domain state.

On POSIX, `activation_start` derives a fresh launch identity from the request
nonce and the child installs it as `HL_LAUNCH_DOMAIN` before engine creation.
`container/vfs.c::launch_reg_publish` stores the live process start time in
`/tmp/.hl-domain.<identity>/b<pid>`. Startup and exec publish the current
process; after fork the parent publishes the child synchronously before fork
returns, closing the child-not-yet-scheduled race, and the child remembers its
own record for exit cleanup. Enumeration and termination accept a record only
when the current process-table start time equals the recorded birth value, so a
reused pid cannot inherit membership. Incomplete publication is skipped by an
observer, while stale or mismatched records are removed by termination.

POSIX termination scans for validated members, sends `SIGKILL` to each pid, and
requires two consecutive empty scans. A live scan resets the empty count. This
is repeated for at most 200 ten-millisecond rounds because a member may be
mid-fork while another scan kills its parent. Success removes all pid, birth,
and executable records, the directory, and the launch-domain network registry.
A missing domain and repeated termination are successful.

On Windows the 128-bit identity names a Job Object. `CreateProcessW` creates the
activation child suspended; it is assigned first to the shared named domain job
and then to the activation-private job before `ResumeThread`. Descendants join
the jobs structurally before they execute, remain members across process-group
changes, and cannot be confused with a reused pid. Enumeration grows and
retries `JobObjectBasicProcessIdList` snapshots. Termination repeatedly calls
`TerminateJobObject` and queries membership until two consecutive snapshots are
empty, with the same 200 by ten-millisecond bound. The named object disappears
after its last handle/member, so a missing object and repeated termination are
successful. Every creation or assignment failure closes process, thread, pipe,
attribute-list, activation-job, and domain-job ownership in reverse order.

The smallest generic Rust boundary is a process-domain capability used by the
engine launcher, not by guest runtime domains:

- extend platform spawning with an optional `Domain`, because Windows must
  assign the suspended process before its first instruction;
- expose a PID-reuse-safe `ProcessIdentity { id, birth }` on POSIX and implement
  atomic birth-record publication from both the spawning parent and every
  host-process fork path;
- expose bounded `processes(domain)` and idempotent `terminate(domain)` methods;
- keep POSIX registry/start-time mechanics in Linux and macOS adapters and
  named Job Object mechanics in the Windows adapter;
- make the domain owner terminate and drain the domain before releasing image,
  network, log, and container state. Non-owning exec launches only join it.

Acceptance needs a real platform test whose in-domain child calls `setsid`,
forks, reports the descendant identity, and lets the initial child exit. A
separately spawned process outside the domain must remain alive after domain
termination. The test must prove the escaped descendant is gone, the unrelated
process survives, repeated termination succeeds, and POSIX registry state is
removed. A deterministic fake-backend unit test must additionally prove that a
live observation between empty observations resets the two-empty-round drain;
one empty snapshot is not sufficient. Windows runs the same behavioral test
through Job Object membership rather than birth files.
