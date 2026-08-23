#![cfg(feature = "native-test-hooks")]

//! What a guest may and may not see in `/proc/<pid>/fdinfo`.
//!
//! `proc_fdinfo_dir_open()` had no covering test of any kind until this file:
//! `grep -rn fdinfo --include=*.rs src tests` matched nothing, while two separate lanes found
//! guest-visible defects in that one function. Both were found by mutating it and watching a
//! hand-built guest fixture, and both fixtures were deleted with the worktrees that made them, so
//! neither defect could have been found a second time.
//!
//! The probe those lanes carried counted the *change* in listing size after creating an eventfd. It
//! stayed green under both defects, because a leaked descriptor and a phantom entry each move the
//! before and after counts together. These cases assert the listing's contents instead, which is
//! the property that actually failed, and each one manufactures the condition it tests rather than
//! hoping the ambient process supplies it -- an ordinary test process has no engine-private
//! descriptors and no emulated eventfd, so a case that merely inspected the current listing would
//! pass against every mutation.
//!
//! The producer is compiled once per guest target, so every case runs on both. That is the
//! asymmetry `reserved_register` exists to catch, and `native_x86` has repeatedly been the arm
//! missing a piece.

const AARCH64: u32 = 1;
const X86_64: u32 = 2;

/// Run one scenario on both guest targets and require a pass from each.
///
/// A negative return is a harness failure -- the scenario could not be set up -- and is reported
/// separately from a positive invariant code, so "this host cannot establish a private descriptor
/// band" can never be mistaken for "the listing concealed it".
#[track_caller]
fn both(scenario: u32, invariant: &str) {
    for (isa, name) in [(AARCH64, "aarch64"), (X86_64, "x86_64")] {
        let verdict = hl_native::proc_fdinfo_listing_test(isa, scenario);
        assert!(
            verdict >= 0,
            "{name} scenario {scenario}: could not establish the fixture (errno {}). This is not a \
             pass -- the invariant '{invariant}' went untested on this host.",
            -verdict
        );
        assert_eq!(verdict, 0, "{name} scenario {scenario}: {invariant}");
    }
}

#[test]
fn every_listed_descriptor_is_open() {
    // 1: the listing was empty, which would satisfy every other case vacuously.
    // 2: a number was listed for a descriptor that is not open -- the shape a phantom entry takes
    //    when the producer publishes a descriptor it opened for its own enumeration and then closed.
    both(0, "the listing named a descriptor that is not open");
}

#[test]
fn nothing_past_the_guest_descriptor_ceiling_is_published() {
    // 3: the fixture failed to place the descriptor above the ceiling. 4: it was published.
    // The fixture deliberately carries NO private-ledger row, so the `fd < HL_NFD` bound is the only
    // mechanism that can conceal it. An adopted descriptor would be concealed by the ledger check as
    // well, and the bound would then have no probe that could fail -- the two mechanisms overlap in
    // production, which is precisely why each needs a case that isolates it.
    both(
        1,
        "a descriptor past the guest's own descriptor ceiling appeared in its fdinfo",
    );
}

#[test]
fn an_open_descriptor_is_listed_and_a_closed_one_is_not() {
    // 5: an open descriptor was missing. 6: a closed one was retained.
    // Concealment is only half the contract; a producer that hid everything would pass the rest.
    both(2, "the listing did not track a descriptor's open and closed states");
}

#[test]
fn the_listing_reaches_the_top_of_the_guest_descriptor_band() {
    // 7: a descriptor high in the guest's own band was missing. This is what stops the listing's
    // bound being "optimised" down to the highest fd the producer happens to expect.
    both(
        3,
        "the listing stopped short of a high descriptor inside the guest band",
    );
}

#[test]
fn an_emulated_eventfds_hidden_peer_stays_hidden() {
    // 8: the hidden pipe write end was published. 9: the eventfd itself vanished with it.
    // This is the one property the deleted probe did cover, kept because it is the only caller for
    // which `eventfd_peer_is_engine_fd`'s negative answer is not redundant with the private ledger.
    both(
        4,
        "the pipe write end behind an emulated eventfd was published to the guest",
    );
}

#[test]
fn a_ledger_registered_descriptor_is_concealed_whatever_its_number() {
    // 10: the fixture's descriptor did not land low enough to be a meaningful test.
    // 11: a descriptor carrying an engine-private ledger row was published to the guest.
    //
    // Concealing by number alone is not enough, and this case is why. The private floor is
    // min(RLIMIT_NOFILE - 4096, 65536) and the listing walks 0..65536, so the two coincide only when
    // RLIMIT_NOFILE is at least 69632. This box runs 1048576 and is safe; a host at the very common
    // 65536 places the whole private band at 61440..65535, inside the range the listing walks. That
    // is reachable on Linux, not only on the Darwin arm that clamps to kern.maxfilesperproc.
    both(
        5,
        "a descriptor with an engine-private ledger row was published to the guest",
    );
}
