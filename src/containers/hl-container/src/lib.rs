//! Embeddable headless Linux container lifecycle.
#![forbid(unsafe_code)]

// This crate is POSIX-only by construction, and says so here rather than letting a
// Windows build discover it 200 lines deep in somebody else's C.
//
// MEASURED 2026-08-21 on x86_64 Linux with the pinned `pkgsCross.mingwW64` toolchain: a
// `--target x86_64-pc-windows-gnu` build of this crate never reaches its own source. It
// stops in `aws-lc-sys`'s build script, which arrives through `hl-images` ->
// `oci-client` -> `rustls`, and the failure text names a missing `sched.h` in a vendored
// jitterentropy header. That message has been read three times as "an include path needs
// scoping"; it is not. `aws-lc/crypto/internal.h` selects `#include <pthread.h>`
// specifically for `defined(__MINGW32__) && !defined(__clang__)`, so aws-lc on mingw-gcc
// structurally requires a winpthreads sysroot ahead of the system headers -- and that is
// the same include position `hl-native`'s `toolchain/msvc-posix` shim must occupy to
// build the C engine. `cc` reads one process-global `CFLAGS_x86_64_pc_windows_gnu`, so
// the two cannot both be satisfied in one `cargo` invocation. Verified in both
// directions: with winpthreads on `CFLAGS` `aws-lc-sys` compiles and `hl-native` dies
// redefining `pthread_cond_timedwait`, `clock_gettime` and `nanosleep`; without it
// `hl-native` compiles and `aws-lc-sys` dies on `pthread.h`. `-idirafter` splits the
// difference and satisfies neither.
//
// None of which is the reason this crate is Unix-only. It is Unix-only because it drives
// Linux namespaces, and `checkpoint/directory.rs` records the second, closer wall: this
// crate and `hl-images` name `std::os::unix::fs` unconditionally in dozens of places.
// The 26 `#[cfg(not(unix))]` arms in `filesystem*.rs` and `checkpoint/directory*.rs`
// compile in no configuration at all, which is why they have rotted; this refusal states
// the constraint they were pretending to satisfy. Removing them is the crate owner's
// call and a separate change.
//
// Written as a refusal rather than an absence because a crate cannot be name-resolution
// error the way `hl_native::process_identity_signal` is on Windows. The rewrite-every-cfg
// technique that `checkpoint/directory.rs` documents still works: it renames
// `cfg(not(unix))` to a name that is never set, and renames this one with it.
#[cfg(not(unix))]
compile_error!(
    "hl-container drives Linux namespaces, bind mounts and POSIX file identity; it has no \
     non-Unix configuration. See the note at the top of src/lib.rs."
);

mod checkpoint;
mod config;
mod console;
mod containers;
mod engine;
mod error;
mod executions;
pub mod filesystem;
mod generation;
mod identity;
mod model;
mod networks;
mod service;
mod storage;
mod volume_size;
mod volumes;

pub use checkpoint::{CheckpointError, CheckpointImage, CheckpointImages};
pub use config::{Config, Persistence};
pub use console::{Input, Session};
pub use containers::{
    Builder, CommitMetadata, Containers, FilesystemUsage, LifecycleAction, LifecycleEvent, LifecycleEvents,
};
pub use error::{Error, Result};
pub use executions::Executions;
pub use filesystem::{Change, ChangeKind, Changes, Extraction, Filesystem, Limits, Stat};
pub use model::{
    Access, BindPropagation, Check, Checkpoint, Console, Container, ContainerId, ContainerSpec, ContainerState,
    Endpoint, EndpointSpec, Entry, Environment, EnvironmentRecord, Exec, ExecId, ExecLifetime, ExecNetwork, ExecSpec,
    ExecState, Execution, ExitStatus, FaultCause, Guest, Health, HealthStatus, Healthcheck, Isolation, Logs, Mount,
    MountSource, Network, NetworkDriver, NetworkId, NetworkMode, NetworkSpec, Port, Probe, Process, Protocol, Prune,
    Publication, RemovalPolicy, Resolver, ResourceLimit, Resources, Restart, RestartPolicy, Rootfs, Sandbox,
    SeccompBaseline, Signal, Size, Stream, Streams, Subnet, Update, Volume, VolumeKind, VolumeSource, VolumeSpec,
    WaitCondition, normalized_mount_target,
};
pub(crate) use model::{JournalId, LogChunk};
pub use networks::Networks;
pub use volumes::Volumes;
