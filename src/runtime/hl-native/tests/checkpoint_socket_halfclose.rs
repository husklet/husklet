#![cfg(feature = "native-test-hooks")]

//! Half-close across a checkpoint.
//!
//! Measured on the bare host kernel (Linux 7.0.11) before any of this was designed, for AF_UNIX
//! `SOCK_STREAM` and `SOCK_SEQPACKET` alike:
//!
//! | survivor's state                          | `recvmsg` | survivor's `send`      |
//! |-------------------------------------------|-----------|------------------------|
//! | peer fully closed                         | `0`       | `-1 EPIPE`             |
//! | peer `shutdown(SHUT_WR)`, peer still open  | `0`       | succeeds, peer reads it |
//! | survivor's own `shutdown(SHUT_RD)`         | `0`       | succeeds                |
//!
//! Three different states, one return value. `recv() == 0` therefore cannot mean "the peer closed", and a
//! capture cannot resolve it with the discriminator a program would use, because sending a probe byte
//! would inject it into the guest's stream. `AF_UNIX SOCK_DGRAM` does not even offer the ambiguity: a
//! closed peer reads `EAGAIN`, never 0.
//!
//! So the state is recorded rather than inferred: each endpoint carries the direction *it* closed, and
//! restore replays that with `shutdown(2)` after the queues are refilled.

/// Capture records the closed direction, per endpoint. Linux exposes half-close through no `getsockopt`,
/// so a record that omits it cannot be replayed by anything downstream, and the mask must belong to the
/// endpoint rather than to the pair: "somebody closed a direction" does not say who may still write.
#[test]
fn each_endpoint_records_the_direction_it_closed_and_not_its_peers() {
    for isa in [1, 2] {
        hl_native::checkpoint_socket_halfclose_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} half-close capture failed at {status}"));
    }
}

/// The defect this exists for. The drain reads 0 from a peer that did `shutdown(SHUT_WR)` and is still
/// open, and used to record `peer_closed`. Restore destroys a peer recorded closed, so a live half-closed
/// client — the state every long-lived PostgreSQL backend connection reaches — came back with no far end
/// at all, and the surviving process saw end of stream on a connection that was never closed.
#[test]
fn a_drain_that_reads_end_of_stream_does_not_record_a_live_peer_as_closed() {
    for isa in [1, 2] {
        hl_native::checkpoint_socket_halfclose_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} peer_closed misinference at {status}"));
    }
}

/// A half-closed endpoint is admissible now that the mask is representable. Capture used to refuse it
/// outright with `ENOTSUP`, and one refused descriptor fails the whole image.
#[test]
fn a_half_closed_endpoint_is_admitted_rather_than_refused() {
    for isa in [1, 2] {
        hl_native::checkpoint_socket_halfclose_test(isa, 2)
            .unwrap_or_else(|status| panic!("ISA {isa} half-close admission failed at {status}"));
    }
}

/// The replay reproduces the measured kernel state through the production mask-to-direction map: the
/// survivor reads end of stream, both descriptors stay open, the survivor can still send, and the peer
/// that closed its write half still receives and still cannot write.
#[test]
fn replaying_the_recorded_mask_reproduces_the_measured_kernel_state() {
    for isa in [1, 2] {
        hl_native::checkpoint_socket_halfclose_test(isa, 3)
            .unwrap_or_else(|status| panic!("ISA {isa} half-close replay failed at {status}"));
    }
}

#[test]
fn checkpoint_socket_halfclose_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_socket_halfclose_test(isa, 4), Err(99));
    }
}
