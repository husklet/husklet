//! The synchronised-I/O open flags are a durability contract: a guest that opens `O_DSYNC` must get a
//! host descriptor that actually carries the barrier, and must be able to read it back through
//! `F_GETFL`. Dropping either direction lets the engine acknowledge a barrier it never issued --
//! `PostgreSQL`'s default `wal_sync_method` on Linux is `open_datasync`, which opens the WAL `O_DSYNC`
//! and then never calls `fdatasync` on it, so every committed transaction rides on this translation.

use std::{fs, path::PathBuf, process::Command};

#[test]
fn guest_synchronised_open_flags_reach_the_host_and_read_back() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-sync-open-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("sync open probe directory");
    let source = scratch.join("sync_open.c");
    let executable = scratch.join("sync_open");
    fs::write(
        &source,
        r#"
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <string.h>
#include "linux_abi/guest_sync.h"

int main(void) {
    /* The guest words are Linux's, arch-independent. */
    if (HL_GUEST_O_DSYNC != 0x1000) return 1;
    if (HL_GUEST_O_SYNC != 0x101000) return 2;

    /* A plain open asks for no barrier and must not acquire one. */
    if (hl_guest_sync_open_flags(O_WRONLY | O_CREAT | O_TRUNC) != 0) return 3;

#if HL_HOST_HAS_SYNC_OPEN
    /* O_DSYNC must map to the host's O_DSYNC -- not to nothing. */
    int dsync = hl_guest_sync_open_flags(O_WRONLY | HL_GUEST_O_DSYNC);
    if (dsync == 0) return 4;
    if (!(dsync & O_DSYNC)) return 5;

    /* O_SYNC is the STRONGER barrier and must never decay to the weaker host flag. Linux forces
     * O_DSYNC on whenever __O_SYNC is set, so __O_SYNC alone still selects the full barrier. */
    if (hl_guest_sync_open_flags(O_WRONLY | HL_GUEST_O_SYNC) != O_SYNC) return 6;
    if (hl_guest_sync_open_flags(O_WRONLY | HL_GUEST_O_FULL_SYNC) != O_SYNC) return 7;

    /* F_GETFL round-trip: the guest must be able to observe the barrier it asked for. */
    if (hl_host_sync_guest_flags(O_WRONLY) != 0) return 8;
    if (hl_host_sync_guest_flags(O_WRONLY | O_DSYNC) != HL_GUEST_O_DSYNC) return 9;
    if (hl_host_sync_guest_flags(O_WRONLY | O_SYNC) != HL_GUEST_O_SYNC) return 10;

    /* End to end through the real kernel: open a real file with the translated word and confirm the
     * descriptor the host hands back genuinely carries the flag. A dropped bit reads back as a plain
     * descriptor, which is exactly the silent-acknowledgement defect this guards. */
    char path[] = "/tmp/hl-sync-open-probeXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 11;
    close(fd);
    fd = open(path, O_WRONLY | hl_guest_sync_open_flags(O_WRONLY | HL_GUEST_O_DSYNC));
    if (fd < 0) { unlink(path); return 12; }
    int back = fcntl(fd, F_GETFL);
    close(fd);
    unlink(path);
    if (back < 0) return 13;
    if (hl_host_sync_guest_flags(back) != HL_GUEST_O_DSYNC) return 14;
#endif
    return 0;
}
"#,
    )
    .expect("sync open probe source");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let built = Command::new(&compiler)
        .args(["-std=c11", "-D_GNU_SOURCE"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .expect("compile sync open probe");
    assert!(built.success(), "sync open probe did not compile");
    let ran = Command::new(&executable).status().expect("run sync open probe");
    assert!(ran.success(), "sync open probe failed with {ran}");
    fs::remove_dir_all(scratch).expect("remove sync open probe directory");
}
