# Application configuration boundaries

`hl-engine` owns configuration capture and validation, but it does not own
runtime algorithms or host process mechanics.

## Launch-config wire

The retained `hl_launch_config` ABI is an architecture-neutral 184-byte
version-one header followed by a bounded byte pool. The parser validates magic,
ABI, declared header and pool sizes, reserved fields, process-domain identity,
network invariants, lower-layer records, publish records, string termination,
and argument termination before producing owned launch input.

The wire carries rootfs, lower layers, working directory, guest uid/gid,
process domain, resource limits, network settings, guest arguments, and the
other per-box options defined by `include/hl/config.h`.

## Activation

Activation selects either the AArch64 or x86-64 engine and carries borrowed
host stdio descriptors. ISA and stdio are deliberately outside the launch wire:
the same launch-config schema is consumed by both engines.

Descriptor zero means “inherit the application stream”; nonzero descriptors
must remain in the public ABI's 32-bit-significant range. Executable and config
paths are owned and absolute.

## Environment and options

Bootstrap environment is captured into an owned value object. Consuming
`HL_ACTIVATION_FD` removes it even when parsing fails, matching the retained C
contract. Engine options use the immutable registry from `options.c`, but all
values and byte accounting are launch-owned; no process or thread globals are
introduced.

## Workspace

A direct-launch workspace is temporary host lifecycle state used to stage and
clean up a launch. It is not public configuration, is not serialized in
`hl_launch_config`, and will belong to the future process/platform adapter that
creates it. The lifecycle coordinator now consumes a narrow `Workspace` port;
native workspace creation remains empty. Keeping it out of configuration
prevents host implementation details from becoming a persistent ABI.

## Runtime plan and lifecycle

`RuntimeLaunchPlan` projects every retained launch field into a launch-owned,
byte-preserving option store. It retains the C precedence rules for cache
disable, host networking, sandbox versus sentry-only mode, checkpoint policy,
and default process-domain network namespace. Host paths, credentials, and
named services remain behind narrow application ports until platform adapters
exist.

Each `Engine` owns its ISA and lifecycle synchronization. It coordinates
created, starting, running, stopping, exited, and destroyed states through
injected `Launcher` and `Workspace` ports. Stop requests made during startup are
replayed after process creation; wait and terminal failures are cached; cleanup
and destroy are idempotent. No backend registry or process/native adapter is
implemented in this stage.

## Runtime composition

`EngineBackend` validates the channels required by the projected checkpoint
mode and asks an injected `RuntimeFactory` for one complete `GuestMachine`.
Factory implementations own runtime-domain construction and must destroy
partially constructed domains in reverse order before reporting failure. This
keeps domain handles and ordering out of application values.

Activation messaging and checkpoint replacement/loading are exposed as safe,
bounded application ports. They contain no descriptors, C pointers, or ambient
process state. A composed machine bridges directly into the per-engine
lifecycle, so AArch64 and x86-64 machines in the same process retain independent
configuration and control state.

The complete production runtime factory, native execution adapter, OS process
adapter, and C ABI remain intentionally incomplete.

The first concrete factory now constructs the available foundational Rust
domains: task registry, descriptor/epoll ownership, event catalog, provider
namespace, and seccomp control. Each assembly is instance-owned and explicitly
tears these resources down in reverse construction order. Projected process
limits configure the task registry before host publication.

Guest execution and host validation remain injected coarse ports. Memory,
network, IPC, Linux personality, loader, complete fork participation, and the
eight-domain checkpoint coordinator do not yet have a constructible production
graph; requesting those capabilities returns `Unsupported` instead of routing
through a fake or partial implementation.
