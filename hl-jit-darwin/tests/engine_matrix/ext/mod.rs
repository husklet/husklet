//! Basics EXTENSION groups — per-category expansion of the in-process JIT matrix, one file per agent so
//! many builders work without collision (mirrors src/scenarios/). The base groups stay in cases/mod.rs;
//! these add breadth. `cases::all()` appends `ext::all()`. Each file keeps itself compiling.

use crate::support::Group;

pub mod abi; // codegen / ABI: int/float/simd/varargs/recursion/jumptable/fnptr/struct-abi
pub mod completeness; // syscall-table + x86-64/aarch64 opcode COMPLETENESS probes (no images)
pub mod darwin; // macOS-native (lighter-touch): kqueue/sysctl/mach/Mach-O ABI corners
pub mod ipc; // pipes/fifo/sysv+posix shm/sem/msg/unix sockets/scm_rights + edge corners
pub mod libc; // string/mem/stdio/malloc/math/locale/time/regex/glob breadth
pub mod linuxsys; // epoll/eventfd/timerfd/signalfd/inotify/sendfile/splice/memfd/pidfd (oracle)
pub mod memory;
pub mod net; // tcp/udp/unix/sockopt/nonblock/sendmsg/poll-loops/half-close
pub mod posix; // file/dir/mmap/poll/signal/process/fs-metadata syscalls (portable + oracle)
pub mod soak; // long-run JIT machinery: code-cache/IBTC/SMC/churn endurance
pub mod syscallcompat; // syscall lifecycle/edge-case regression probes (docs/bugs/syscall-compat.md)
pub mod threads; // mutex/condvar/rwlock/barrier/atomics/TLS/futex contention // RSS leak / sustainability probes (guest-visible memory growth over churn)
                 // compatibility-coverage expansion (one file per category, each self-owned):
pub mod clitools; // real CLI tools (busybox coreutils) in the alpine rootfs
pub mod dentry; // positive dentry/path-resolution cache: mutation<->lookup coherence storms
pub mod elf210; // #210: x86_64 ELF-loader fixed-base (PC_IMG_BASE) collision -> kernel-base fallback (no exit)
pub mod execfaultx; // fork->execve child faults + CRASHDBG Mach exception-port guest-fault delivery
pub mod forkx; // preserved-arena fork: 1000-fork mixed-pattern storm (exit/work/cold/exec/nested)
pub mod fs;
pub mod fsx; // extended fs: *at() family, xattr, fadvise/close_range/O_PATH
pub mod gpu_render_ir; // direct dd-gpu compositor replay probes: offscreen target -> sampled final target
pub mod gui; // EGL/Wayland GL-shim probes for renderer debugging
pub mod isolation; // container isolation + resource fidelity: --cpus/--read-only/--ulimit, masked/ro /proc paths
pub mod ltpgaps; // LTP gap cluster: dup/fcntl flags, link/lstat, socket error paths, prctl/nanosleep/sched/read
pub mod memx; // vm: MAP_FIXED, mlock, mremap, mincore
pub mod pcachex; // persistent translated-code cache: fork/exec/thread lifecycle under DDJIT_PCACHE=1
pub mod processx; // process: posix_spawn/vfork/waitid/getrusage/prlimit/clone3/futex
pub mod procexe; // /proc/self/exe + readlink/readlinkat surface: exe canonicalization, magic links, execveat
pub mod procfs; // /proc /sys /dev pseudo-file CONTENT conformance + permission/mode fidelity (zero-stub gate)
pub mod scratchx; // scratch/distroless loader-exec: a static binary as the sole file in an empty rootfs
pub mod signalx; // signals: sigaltstack/SA_RESTART/itimer/pause/sigwait/siginfo/tgkill
pub mod timex; // clocks: clock_getres/gettimeofday/clock_nanosleep/linux clock ids // FILESYSTEM/STORAGE: statfs f_type+geometry per mount, /proc/mounts+mountinfo+filesystems

pub fn all() -> Vec<Group> {
    let mut g = vec![];
    g.extend(memory::groups());
    g.extend(abi::groups());
    g.extend(libc::groups());
    g.extend(posix::groups());
    g.extend(linuxsys::groups());
    g.extend(threads::groups());
    g.extend(ipc::groups());
    g.extend(net::groups());
    g.extend(soak::groups());
    g.extend(syscallcompat::groups());
    g.extend(darwin::groups());
    g.extend(completeness::groups());
    g.extend(fsx::groups());
    g.extend(gui::groups());
    g.extend(gpu_render_ir::groups());
    g.extend(memx::groups());
    g.extend(signalx::groups());
    g.extend(processx::groups());
    g.extend(timex::groups());
    g.extend(clitools::groups());
    g.extend(isolation::groups());
    g.extend(procfs::groups());
    g.extend(procexe::groups());
    g.extend(pcachex::groups());
    g.extend(forkx::groups());
    g.extend(execfaultx::groups());
    g.extend(dentry::groups());
    g.extend(elf210::groups());
    g.extend(ltpgaps::groups());
    g.extend(scratchx::groups());
    g.extend(fs::groups());
    g
}
