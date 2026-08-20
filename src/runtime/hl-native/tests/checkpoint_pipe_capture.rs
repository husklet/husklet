#![cfg(feature = "native-test-hooks")]

//! Pipe capture is destructive: the only way to observe a pipe's buffered bytes is to read them out of the
//! kernel, and they are then gone for every process that holds that pipe. That is only sound under the
//! freeze `ckpt_dump_self` now establishes -- every member stopped, and held alive in its park until the
//! coordinator releases it -- because otherwise a sibling writes into the pipe after the drain and the
//! image reports success having lost those bytes.
//!
//! These fixtures drive the real capture path against real kernel pipes. They pin the three things the
//! freeze does not decide on its own: who drains, that the bytes survive the round trip in order, and which
//! processes a post-drain abort is terminal for.

/// An inherited pipe held by three processes. Exactly one wins the image-wide election and drains; the
/// other two return immediately; the published object is the written payload, byte for byte and in order;
/// and the pipe is empty afterwards, so no second drain could find anything.
#[test]
fn one_holder_of_an_inherited_pipe_drains_it_and_publishes_every_byte_in_order() {
    for isa in [1, 2] {
        hl_native::checkpoint_pipe_capture_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} inherited-pipe capture failed at {status}"));
    }
}

/// The abort contract. A drain empties the pipe for every holder at once, so a capture abandoned after the
/// drain is terminal for the winner AND for every co-holder that returned without reading a byte. Marking
/// only the winner let the losers resume out of their park onto a silently emptied pipe -- and an inherited
/// pipe has many holders by definition, which is the shape this path exists for.
#[test]
fn every_holder_of_a_drained_pipe_is_terminal_on_abort_not_only_the_winner() {
    for isa in [1, 2] {
        hl_native::checkpoint_pipe_capture_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} post-drain abort contract failed at {status}"));
    }
}

/// A write-only end cannot read the buffer, so it must never take the claim: winning an election it cannot
/// satisfy would publish an empty object and strand the bytes in the kernel.
#[test]
fn a_write_only_pipe_end_neither_claims_nor_drains() {
    for isa in [1, 2] {
        hl_native::checkpoint_pipe_capture_test(isa, 2)
            .unwrap_or_else(|status| panic!("ISA {isa} write-end exclusion failed at {status}"));
    }
}

/// Both capture paths ask one question. The queued-rights path used to ask `== O_RDONLY` while the image
/// path asked `!= O_WRONLY`, so an `O_RDWR` end reached through `SCM_RIGHTS` published no object at all and its
/// pipe was restored empty on a checkpoint that reported success.
#[test]
fn pipe_drain_eligibility_is_not_write_only_rather_than_read_only() {
    for isa in [1, 2] {
        hl_native::checkpoint_pipe_capture_test(isa, 3)
            .unwrap_or_else(|status| panic!("ISA {isa} drain eligibility failed at {status}"));
    }
}

/// The closed round trip end to end: the bytes the drain removed from a pipe are the bytes the production
/// restore refill puts back into a fresh one, in the same order, with nothing left over. This is the claim
/// `ckpt_capture_pipe_reason` makes in its own comment, driven rather than asserted.
#[test]
fn the_bytes_a_drain_removes_are_the_bytes_restore_puts_back_in_order() {
    for isa in [1, 2] {
        hl_native::checkpoint_pipe_capture_test(isa, 4)
            .unwrap_or_else(|status| panic!("ISA {isa} pipe round trip failed at {status}"));
    }
}

/// THE PRODUCT PROPERTY. A checkpoint refused for a reason that consumes nothing must leave the container
/// running unharmed. The descriptor set is walked in ascending guest-fd order, so this drives a drainable
/// pipe holding a payload BELOW a descriptor the scan refuses outright, and asserts what the guest and the
/// coordinator observe afterwards: every buffered byte still in the pipe, and a disposition that resumes
/// rather than the terminal `cannot resume: its capture was destructive and was not published`.
///
/// Refusals of exactly this shape -- an unsupported socket, a sandbox policy, a missing `SysV` section -- are
/// ordinary. Before the admission and consumption passes were separated, every one of them killed the whole
/// process tree over a policy decision that had destroyed nothing.
#[test]
fn a_refusal_that_consumes_nothing_leaves_every_member_resumable() {
    for isa in [1, 2] {
        hl_native::checkpoint_pipe_capture_test(isa, 5)
            .unwrap_or_else(|status| panic!("ISA {isa} refused-capture survival failed at {status}"));
    }
}

#[test]
fn checkpoint_pipe_capture_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_pipe_capture_test(isa, 6), Err(99));
    }
}
