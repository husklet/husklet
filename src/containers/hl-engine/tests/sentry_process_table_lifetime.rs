#![cfg(target_os = "linux")]
//! The lifetime of the sentry's per-process virtual descriptor table, under the production sandbox default.
//!
//! `Sandbox::SentryOnly` sets `HL_UNTRUSTED`, and the sentry then keys one table per worker PROCESS by the
//! HOST pid every request from that worker carries. Two properties have to hold together and neither is
//! visible without a guest process tree:
//!
//! * the table is released when its process dies, whichever route collects the corpse -- otherwise it holds
//!   that process's duplicated descriptors forever and occupies one of the sentry's bounded process slots;
//! * a host pid the kernel has reissued names a different process, and the fork lane must serve it. Measured
//!   on this x86_64 box: the kernel reissued a specific freed pid after 40.9 s of ordinary fork churn, so
//!   this is a matter of container uptime, not of probability.

use hl_engine::{activation::GuestIsa, launcher::plan::RuntimePlan, options::Options, runtime::Engine};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fixture(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sentry_process_table_lifetime.c");
    let output = directory.join("sentry-process-table-lifetime");
    let compiler = "x86_64-linux-gnu-gcc";
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-std=gnu11", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

/// Runs the fixture under the sandbox default and answers its guest exit status.
fn sandboxed(executable: &Path, arguments: &[&str]) -> i32 {
    let mut options = Options::default();
    options.set("HL_UNTRUSTED", "1", true).unwrap();
    let mut argv = vec![executable.as_os_str().as_encoded_bytes().to_vec()];
    argv.extend(arguments.iter().map(|argument| argument.as_bytes().to_vec()));
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: argv,
        environment: Vec::new(),
        result_path: None,
        options,
    };
    let engine = Engine::from_plan(GuestIsa::X86_64, plan).expect("sandboxed launch");
    engine.start().expect("guest start");
    engine.wait().expect("guest wait").guest_status
}

/// `wait4(2)` had a sentry lane and `waitid(2)` did not, so a guest that ends a child with a signal -- the
/// child then never publishes its own exit -- and collects it with `waitid` freed the host pid while
/// leaving the table filed under it. One child is alive at a time here, so the only thing 200 rounds can
/// exhaust is entries that outlived their processes; before the lane existed this stopped at 63 with
/// `EAGAIN`.
#[test]
fn a_child_collected_with_waitid_releases_its_descriptor_table() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(
        sandboxed(&executable, &["rounds-waitid"]),
        0,
        "a signalled child collected with waitid left its descriptor table behind"
    );
}

/// The third route that frees the pid a table is keyed on, and the only one with no syscall to route:
/// `SA_NOCLDWAIT` asks Linux to leave no zombie, so the auto-reap runs inside a HOST SIGNAL HANDLER --
/// `waitpid(-1, WNOHANG)` in `signal.c` -- and the guest never calls wait at all. The handler cannot
/// publish the release itself: `sentry_ctl_op` spins on the ring's `busy` producer flag, which the very
/// thread the signal interrupted may already hold, so the handler would wait for itself. The release is
/// therefore recorded in a lock-free pending array and published from ordinary syscall context.
///
/// One child is alive at a time here, so the only thing 200 rounds can exhaust is entries that outlived
/// their processes; before the pending array existed this stopped at 63 with `EAGAIN`.
#[test]
fn a_child_auto_reaped_under_sa_nocldwait_releases_its_descriptor_table() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(
        sandboxed(&executable, &["rounds-nocldwait"]),
        0,
        "a child collected by the SA_NOCLDWAIT auto-reap left its descriptor table behind"
    );
}

/// The window the deferred publish exists for, and the reason the handler cannot simply publish. A worker
/// thread inside a forwarded syscall holds the ring's producer flag across the whole round-trip and is
/// parked waiting for the sentry's answer. The SA_NOCLDWAIT auto-reap interrupts THAT thread, so a
/// release published through `sentry_ctl_op` from the handler asks the interrupted thread for a flag only
/// the interrupted thread can return -- measured on this x86_64 box, the guest wedges in the first round
/// and never returns. Recording the pid in the pending array and publishing it from ordinary syscall
/// context takes no flag at all.
///
/// The round's other child is killed by a sibling, so it never publishes its own exit and only the
/// auto-reap route can release its table; 70 rounds therefore outlives the sentry's 63 process slots as
/// well, and one program answers both questions: the guest is not wedged, and the table was released.
#[test]
fn a_child_auto_reaped_while_the_guest_is_parked_in_a_forwarded_syscall_releases_its_table() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(
        sandboxed(&executable, &["blocked"]),
        0,
        "a child that died while the guest was parked in a forwarded syscall wedged it or leaked its table"
    );
}

/// One SIGCHLD can stand for several dead children -- Linux coalesces a standard signal that is already
/// pending -- so the auto-reap collects a whole batch inside a single handler entry and the sentry has to
/// hear about every pid in it. That is what makes the handler's pending record an array rather than one
/// slot, and it is the only scenario here in which more than one release is outstanding at a time.
///
/// A batch of eight is alive at a time, far inside the sentry's 63-slot bound, and 20 rounds is 160
/// children, so the only thing that can exhaust the sentry is entries that outlived their processes.
#[test]
fn a_batch_auto_reaped_under_one_coalesced_sigchld_releases_every_descriptor_table() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(
        sandboxed(&executable, &["batch-nocldwait"]),
        0,
        "a coalesced SIGCHLD left a corpse uncollected or left its descriptor table behind"
    );
}

/// The bound on tables held for SIMULTANEOUSLY live children is fail-closed and must stay where it is: a
/// fork past it is refused with `EAGAIN`, never served out of a slot something else still owns. This is the
/// control for every rounds test above -- without it, "200 rounds completed" would also be satisfied by a
/// sentry that had stopped bounding anything, and it is what catches a harness that never applied the
/// sandbox default at all: `HL_UNTRUSTED` is a launch option with no environment importer, and a run
/// missing it reads `created=70` here while every other test in this file reads a vacuous pass.
#[test]
fn the_bound_on_simultaneously_live_children_is_unchanged() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(
        sandboxed(&executable, &["bound"]),
        0,
        "the sentry no longer refuses the 64th simultaneously live child with EAGAIN"
    );
}

/// A host pid the kernel reissues names a new process, and the fork lane holds the proof: `clone(2)` just
/// handed this worker that number, so no other live process can own it and any entry filed under it is a
/// corpse. The lane must reclaim it and serve the fork. Refusing instead returned `-EEXIST`, which
/// `clone(2)` cannot return on Linux, from an ordinary container fork.
///
/// Rather than churn 4.2 million pids, the harness re-arms `ns_last_pid` on the number the guest leaked,
/// once per guest attempt. That needs privilege, and every statement in the arming loop has to avoid
/// spawning a process -- one intervening fork consumes the pid being armed.
#[test]
fn a_fork_onto_a_reissued_host_pid_is_served() {
    let Ok(last_pid) = std::fs::OpenOptions::new()
        .write(true)
        .open("/proc/sys/kernel/ns_last_pid")
    else {
        println!(
            "SKIP a_fork_onto_a_reissued_host_pid_is_served: /proc/sys/kernel/ns_last_pid is not writable \
             here, so a reissued pid cannot be arranged without churning the whole pid space"
        );
        return;
    };
    drop(last_pid);
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let handshake = work.path().to_path_buf();
    let arming = std::thread::spawn(move || {
        let victim = handshake.join("victim");
        let go = handshake.join("go");
        let stop = handshake.join("stop");
        let mut armed = String::new();
        while !stop.exists() {
            let Ok(request) = std::fs::read_to_string(&victim) else {
                continue;
            };
            if request.trim().is_empty() || request == armed {
                continue;
            }
            let Some(number) = request.split_whitespace().next().and_then(|f| f.parse::<i32>().ok()) else {
                continue;
            };
            armed = request;
            let _ = std::fs::write("/proc/sys/kernel/ns_last_pid", format!("{}", number - 1));
            let _ = std::fs::write(&go, []);
        }
    });
    let status = sandboxed(&executable, &["collide", &work.path().to_string_lossy()]);
    std::fs::write(work.path().join("stop"), []).unwrap();
    arming.join().unwrap();
    assert_ne!(
        status, 7,
        "a fork onto a reissued host pid was refused rather than served"
    );
    assert!(
        status == 0 || status == 6,
        "the collision fixture failed for its own reasons: guest status {status}"
    );
}
