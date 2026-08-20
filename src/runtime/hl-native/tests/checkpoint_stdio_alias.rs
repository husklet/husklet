#![cfg(feature = "native-test-hooks")]

//! The runtime around the engine owns stdin, stdout and stderr, and hands a restored engine a fresh
//! bridge for them. The capture scan exempted them from the "shared pipe restore is not yet supported"
//! refusal by testing the descriptor NUMBER, which is not what identifies an open file description.
//!
//! busybox ash's `savefd` moves the stdout it is about to redirect to the first free descriptor at or
//! above 10 for the duration of `printf x >> file`. A capture landing inside that microsecond window saw
//! the runtime's own stdout pipe at guest fd 10 and refused the ENTIRE checkpoint -- so closing a
//! workspace failed intermittently, more often under load, and the user lost the close.
//!
//! These fixtures drive the real `ckpt_capture_typed_fd` against a real guest descriptor table over real
//! kernel pipes, in that exact shape.

/// The failure verbatim: guest fd 10 duplicates the open file description behind guest fd 1, which is the
/// runtime's stdout pipe. It is the same object already exempted at fd 1, so it is captured as stdio and
/// records WHICH standard descriptor it duplicates, rather than refusing the capture.
#[test]
fn a_duplicate_of_the_runtime_stdout_at_a_high_descriptor_is_captured_as_stdio() {
    for isa in [1, 2] {
        hl_native::checkpoint_stdio_alias_capture_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} stdio-alias capture failed at {status}"));
    }
}

/// The half that keeps the fix from being a widening. A pipe the GUEST created holds an open file
/// description of its own, shares it with no standard descriptor, and must still refuse the capture --
/// restoring it as a duplicate of the runtime's bridge would silently reconnect guest data flow to the
/// runtime's logs. The refusal line this scenario prints to stderr is its expected output.
#[test]
fn a_pipe_the_guest_created_is_still_refused() {
    for isa in [1, 2] {
        hl_native::checkpoint_stdio_alias_capture_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} guest-pipe refusal failed at {status}"));
    }
}

#[test]
fn the_stdio_alias_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_stdio_alias_capture_test(isa, 2), Err(-22));
    }
}
