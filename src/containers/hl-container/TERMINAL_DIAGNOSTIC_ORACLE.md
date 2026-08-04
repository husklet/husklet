# Terminal runtime completion oracle

Audited retained implementations (read-only):

- `../engine/src/host/linux/host.c`, `hl_linux_process_wait`: one waiter owns
  `waitpid`, retries `EINTR`, stores the typed exit kind/value under the host lock,
  broadcasts completion, and every later waiter receives the cached result.
- `../engine/src/host/macos/host.c`, `hl_macos_process_wait`: the same ownership,
  lock, waiter-count, reap, cache, and broadcast lifecycle; host wait errors remain
  status errors rather than becoming synthetic guest exits.

There is no architecture branch in this lifecycle. The macOS adapter translates
guest signals when terminating a process, but completion caching is otherwise
host-equivalent. Closing is rejected until the process was reaped and all waiters
left, then cached state is cleared.

Rust mapping:

| Oracle capability | Rust owner | State |
|---|---|---|
| single wait/reap owner | `service/container/launch.rs`, `Service::own` | implemented |
| durable typed exit result | `ContainerState::{Exited,Restarting}` | implemented |
| durable runtime wait failure | `Container::runtime_diagnostic` | implemented here |
| broadcast one completion to many waiters | `Service::finish`, `Notify` | implemented |
| preserve completion across service restart | container storage record | implemented here |
| clear cached completion for a new process | `Service::launch_locked` | implemented here |
| clear cached completion at teardown | container record removal | implemented |

The fallback `ExitStatus::Fault` remains solely as restart-policy classification;
the persisted diagnostic takes precedence at the public wait boundary, so it is
never reported as the process result when the runtime wait itself failed.
