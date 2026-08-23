//! Byte-range lock contention between two containers sharing one volume.

use crate::api::support::require;
use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Isolation, Mount, Process, Sandbox, VolumeSpec};
use std::path::Path;
use std::time::{Duration, Instant};

/// Holds a write lock, probes it without blocking, then waits for it.
///
/// `hold` reports readiness on stdout before sleeping so the peers only race the
/// lock, never the container start.
const LOCKER: &str = r#"
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int range(int fd, int type, int command, off_t start) {
    struct flock lock;
    memset(&lock, 0, sizeof lock);
    lock.l_type = type;
    lock.l_whence = SEEK_SET;
    lock.l_start = start;
    lock.l_len = 16;
    return fcntl(fd, command, &lock);
}

static void say(const char *text) {
    write(1, text, strlen(text));
}

int main(int argc, char **argv) {
    if (argc < 3) {
        return 2;
    }
    int fd = open(argv[2], O_RDWR | O_CREAT, 0644);
    if (fd < 0) {
        say("OPENFAIL\n");
        return 3;
    }
    if (strcmp(argv[1], "hold") == 0) {
        if (range(fd, F_WRLCK, F_SETLK, 0) != 0) {
            say("HOLDFAIL\n");
            return 4;
        }
        say("HELD\n");
        sleep(atoi(argv[3]));
        say("RELEASED\n");
        return 0;
    }
    if (strcmp(argv[1], "try") == 0) {
        say(range(fd, F_WRLCK, F_SETLK, 0) == 0 ? "OVERLAP=ACQUIRED\n" : "OVERLAP=BLOCKED\n");
        /* A disjoint range must still be granted: tier 2 must conflict on overlap,
           not on the file. */
        say(range(fd, F_WRLCK, F_SETLK, 64) == 0 ? "DISJOINT=ACQUIRED\n" : "DISJOINT=BLOCKED\n");
        return 0;
    }
    if (strcmp(argv[1], "wait") == 0) {
        if (range(fd, F_WRLCK, F_SETLKW, 0) != 0) {
            say("WAITFAIL\n");
            return 5;
        }
        say("ACQUIRED\n");
        return 0;
    }
    return 2;
}
"#;

const HOLD_SECONDS: u64 = 6;

pub(crate) async fn run(work: &Path, rootfs: &Path) -> Result<(), Box<dyn std::error::Error>> {
    compile(work, &rootfs.join("locker"))?;
    let containers = Containers::builder(Config::new(work.join("shared-lock-state")))
        .build()
        .await?;
    containers.volumes().create(VolumeSpec::new("locked")).await?;
    spawn(&containers, rootfs, "lock-holder", &["hold", "/data/db", "6"]).await?;
    containers.start("lock-holder").await?;
    await_held(&containers).await?;

    // A second container must see the first container's lock. Before the tier-2
    // registry both won it, because each coordinator only saw its own records.
    spawn(&containers, rootfs, "lock-probe", &["try", "/data/db"]).await?;
    containers.start("lock-probe").await?;
    let probe = containers.wait("lock-probe").await?;
    let observed = String::from_utf8(containers.logs("lock-probe").await?.stdout)?;
    require(
        probe == ExitStatus::Code(0) && observed.contains("OVERLAP=BLOCKED"),
        &format!("cross-container F_SETLK was granted over a held write lock: {observed:?}"),
    )?;
    require(
        observed.contains("DISJOINT=ACQUIRED"),
        &format!("tier 2 refused a disjoint range, so it conflicts on the file not the bytes: {observed:?}"),
    )?;

    // Blocking acquisition must park until the holder exits, then be woken.
    spawn(&containers, rootfs, "lock-waiter", &["wait", "/data/db"]).await?;
    let started = Instant::now();
    containers.start("lock-waiter").await?;
    let waiter = containers.wait("lock-waiter").await?;
    let waited = started.elapsed();
    let granted = String::from_utf8(containers.logs("lock-waiter").await?.stdout)?;
    require(
        waiter == ExitStatus::Code(0) && granted.contains("ACQUIRED"),
        &format!("cross-container F_SETLKW never acquired: {granted:?}"),
    )?;
    // Without real contention the wait returns immediately; this is what keeps the
    // case from passing vacuously.
    require(
        waited >= Duration::from_secs(2),
        &format!("F_SETLKW returned in {waited:?}, so it never actually blocked"),
    )?;
    require(
        containers.wait("lock-holder").await? == ExitStatus::Code(0),
        "holder did not exit cleanly",
    )?;
    Ok(())
}

async fn spawn(
    containers: &Containers,
    rootfs: &Path,
    name: &str,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/locker").args(args.iter().copied()))
                .name(name)
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                })
                .mount(Mount::volume_read_write("locked", "/data")),
        )
        .await?;
    Ok(())
}

async fn await_held(containers: &Containers) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..u32::try_from(HOLD_SECONDS).unwrap_or(6) * 40 {
        if String::from_utf8_lossy(&containers.logs("lock-holder").await?.stdout).contains("HELD") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("holder never reported acquiring its write lock".into())
}

/// Builds the probe for the guest architecture selected by the caller.
///
/// The pinned dev shell cross compiler intentionally ships no static libc.
/// Prefer the system compiler that owns the host static libc; `HL_GUEST_CC`
/// remains the explicit cross-compilation override.
fn compile(work: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = work.join("locker.c");
    std::fs::write(&source, LOCKER)?;
    let override_cc = std::env::var("HL_GUEST_CC").ok();
    let candidates = override_cc
        .as_deref()
        .map_or_else(|| vec!["/usr/bin/cc", "/usr/bin/gcc"], |value| vec![value]);
    for candidate in &candidates {
        let built = std::process::Command::new(candidate)
            .args(["-static", "-w", "-O2", "-std=gnu11", "-o"])
            .arg(destination)
            .arg(&source)
            .status();
        if built.is_ok_and(|status| status.success()) {
            return Ok(());
        }
    }
    Err(format!("no compiler in {candidates:?} could statically link the guest lock probe").into())
}
