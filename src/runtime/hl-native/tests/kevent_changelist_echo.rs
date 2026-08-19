//! The Linux `kevent` shim must report a per-change failure the way BSD kqueue does: as an `EV_ERROR`
//! echo in the eventlist, with the rest of the changelist still applied.
//!
//! The engine's deferred changelist (`ep_flush`) submits an `EV_ADD`/`EV_DELETE` batch that routinely
//! names a descriptor whose registration is already gone -- a peer thread retired it, or a checkpoint
//! restore rebuilt the armed maps without a matching shim registration. `epoll_append_kevents` is
//! written to skip the resulting echoes. A shim that fails the whole `kevent` call instead turns one
//! stale change into a guest-visible `epoll_wait` errno and strands every change queued behind it.
//! Observed on the `PostgreSQL` checkpoint round trip as `ERROR: epoll_wait() failed: No such file or
//! directory` on every backend forked after a restore.

#![cfg(target_os = "linux")]

use std::{fs, path::Path, process::Command};

const PROBE: &str = r#"
#define _GNU_SOURCE
#include <stdint.h>
static int hl_host_process_fd_private_adopt(int fd);
static void hl_host_process_fd_private_remove(int fd);
#include "host/native_compat.h"
#include <sys/socket.h>
#include <unistd.h>
static int hl_host_process_fd_private_adopt(int fd) { return fd; }
static void hl_host_process_fd_private_remove(int fd) { (void)fd; }

int main(void) {
    struct timespec immediate = {0, 0};
    int pair[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) return 1;
    int queue = kqueue();
    if (queue < 0) return 2;

    /* A delete of a filter that was never registered is a per-change error, not a call failure. */
    struct kevent events[4];
    struct kevent stale;
    EV_SET(&stale, pair[0], EVFILT_WRITE, EV_DELETE, 0, 0, NULL);
    int ready = kevent(queue, &stale, 1, events, 4, &immediate);
    if (ready != 1) return 3;
    if ((events[0].flags & EV_ERROR) == 0) return 4;
    if (events[0].data != ENOENT) return 5;
    if ((int)events[0].ident != pair[0] || events[0].filter != EVFILT_WRITE) return 6;

    /* The changes queued behind the failing one are still applied. */
    struct kevent batch[2];
    EV_SET(&batch[0], pair[0], EVFILT_WRITE, EV_DELETE, 0, 0, NULL);
    EV_SET(&batch[1], pair[0], EVFILT_READ, EV_ADD, 0, 0, NULL);
    ready = kevent(queue, batch, 2, events, 4, &immediate);
    if (ready != 1 || (events[0].flags & EV_ERROR) == 0) return 7;
    if (write(pair[1], "x", 1) != 1) return 8;
    ready = kevent(queue, NULL, 0, events, 4, &immediate);
    if (ready != 1 || events[0].filter != EVFILT_READ || (int)events[0].ident != pair[0]) return 9;

    /* With no room for the echo the call still fails closed, exactly as BSD documents. */
    EV_SET(&stale, pair[1], EVFILT_WRITE, EV_DELETE, 0, 0, NULL);
    if (kevent(queue, &stale, 1, NULL, 0, &immediate) != -1 || errno != ENOENT) return 10;

    close(pair[0]);
    close(pair[1]);
    close(queue);
    return 0;
}
"#;

#[test]
fn stale_changelist_entry_echoes_ev_error_without_failing_the_call() {
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let scratch = tempfile::tempdir().expect("create kevent changelist probe directory");
    let source = scratch.path().join("kevent_changelist_echo.c");
    let executable = scratch.path().join("kevent_changelist_echo");
    fs::write(&source, PROBE).expect("write kevent changelist probe");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(&native)
        .arg("-I")
        .arg(native.join("include"))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile kevent changelist probe");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("run kevent changelist probe");
    assert!(run.success(), "kevent changelist probe failed with {run}");
}
