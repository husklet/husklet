#![cfg(all(unix, feature = "native-test-hooks"))]

//! `O_APPEND` arriving after the open is the ordinary case, not the exotic one: `fcntl(F_SETFL)` is a
//! change to the open file description, and descriptors 0, 1 and 2 are the ones a guest is handed rather
//! than asks for. GNU make sets `O_APPEND` on stdout before writing, so `make --version | cat` reported
//! `make: write error: stdout` and exited 1 whenever stdout was a pipe, a file or `/dev/null` -- and a
//! guest that set it on fd 2 lost its own stderr. Linux fails none of those writes.
//!
//! These fixtures drive the real `hl_linux_fcntl`/`hl_linux_write` pair -- the two syscalls the guest
//! issues -- against a real guest descriptor table over real kernel objects, with fd 1 adopted from the
//! launching process exactly as a launch adopts it.

/// The headline shape: an adopted stdout that is a pipe. Linux accepts `O_APPEND` on a pipe and ignores
/// it, because a pipe has no position, so the write must succeed and the bytes must reach the reader.
#[test]
fn an_adopted_pipe_accepts_o_append_and_still_writes() {
    hl_native::setfl_append_write_test(0).expect("F_SETFL(O_APPEND) write on an adopted pipe");
}

/// The half that keeps the fix from being a widening. When the adopted descriptor IS positional -- stdout
/// redirected to a file the parent opened without `O_APPEND` -- `O_APPEND` is not decoration: Linux moves
/// the write to EOF even though the position was rewound to 0, so the file must read `AAABB` and not
/// `BBA`. Answering this one with a plain write would pass the pipe fixture and corrupt a log.
#[test]
fn an_adopted_regular_file_appends_at_end_of_file_after_the_flag_is_set() {
    hl_native::setfl_append_write_test(1).expect("F_SETFL(O_APPEND) write on an adopted regular file");
}

/// `writev` reaches the appending path through its own host operation, so it needs its own evidence.
#[test]
fn an_adopted_pipe_accepts_o_append_on_the_vector_write() {
    hl_native::setfl_append_write_test(2).expect("F_SETFL(O_APPEND) writev on an adopted pipe");
}

#[test]
fn the_setfl_append_hook_rejects_unknown_scenarios() {
    assert_eq!(hl_native::setfl_append_write_test(3), Err(-22));
}
