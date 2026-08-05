# In-process cancellation audit

The retained C oracle was inspected before changing the Rust cancellation path.

- `../engine/src/core/activation.c`: `hl_activation_domain_terminate` durably
  records termination and repeatedly reaps the launch domain. Termination is
  idempotent and does not depend on a thread already being registered.
- `../engine/src/core/dispatch.c`: `run_guest` observes the CPU `exited` and
  `irq` fields at translated execution boundaries.
- `../engine/src/linux_abi/thread.c`: `thread_target_signal` publishes pending
  state before setting `irq`; it then wakes a published condition wait or sends
  `THREAD_INT_SIG` to interrupt a blocking host syscall. `thread_exit_others`
  repeats that ordering while waiting for peers to unregister.

Rust ownership maps as follows:

| C capability | Rust owner | Result |
|---|---|---|
| durable termination before thread registration | `ThreadSet` | cancellation signal is latched under the set mutex and inherited by later publications |
| translated-code interrupt | `InterruptToken` | set for every live and newly published run |
| blocking host-wait interrupt | `readiness::Cancellation` | eventfd and interruption wake retained |
| idempotent terminal signal | `ThreadSet` | first signal wins |
| terminal failure publication | `GuestExecutor` state | failure and running-entry removal occur in one locked state transition |

The ordering is deliberate: cancellation becomes durable while holding the same
mutex used for machine publication. A machine therefore observes either the
pre-existing cancellation during publication or a later cancellation while it
is already present; there is no empty-set window in which a stop can disappear.
