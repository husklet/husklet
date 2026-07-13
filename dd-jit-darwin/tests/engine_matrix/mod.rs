//! The test registry — declarative groups of cases. Add a case by adding a line.
//!
//! `src(name, file)`         compile `guests/<file>` (aarch64) and run it bare.
//! `.oracle()`               diff the JIT run's stdout+exit against running the same binary natively.
//! `.exit(n)/.out(s)/.has(s)` golden checks.
//! `in_rootfs(name, img, a)` run a program already inside an image's rootfs (container behaviour).
//! `fixture(name, &[(e,p)])` run a prebuilt binary on engine `e` (the only way to exercise x86-64 now).
//!
//! The case-builder functions are grouped into per-category sibling modules; `all()` aggregates them.

use crate::support::{in_rootfs, Case, Group};

pub mod ext; // per-category basics expansion (one file per agent, appended below)

mod abi;
mod container;
mod net;
mod proc;
mod regress;
mod syscall;
mod workload;

use abi::*;
use container::*;
use net::*;
use proc::*;
use regress::*;
use syscall::*;
use workload::*;

/// Every group, in display order. Base groups here + the per-agent extension groups in `ext`.
pub fn all() -> Vec<Group> {
    let mut g = vec![
        compat(),
        libc(),
        system(),
        net(),
        proc(),
        threads(),
        posix(),
        ipc(),
        clib(),
        linuxsys(),
        heavy(),
        soak(),
        edge(),
        compile(),
        realsw(),
        containersw(),
        perf(),
        busybox(),
        container(),
        sandbox(),
        x86(),
        darwin(),
        regress(),
    ];
    g.extend(ext::all());
    g
}

/// Run `sh -c <cmd>` inside the alpine rootfs (the workhorse for container/busybox/sandbox cases).
fn sh(name: &'static str, cmd: &'static str) -> Case {
    in_rootfs(name, "alpine", &["/bin/sh", "-c", cmd])
}
