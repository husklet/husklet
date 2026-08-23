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
//!   on this `x86_64` box: the kernel reissued a specific freed pid after 40.9 s of ordinary fork churn, so
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

/// The bound on tables held for SIMULTANEOUSLY live children is fail-closed and must stay where it is: a
/// fork past it is refused with `EAGAIN`, never served out of a slot something else still owns. This is the
/// control for the test above -- without it, "200 rounds completed" would also be satisfied by a sentry
/// that had stopped bounding anything.
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
