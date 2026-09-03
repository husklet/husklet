#[test]
fn admitted_direct_jmp_successor_is_decoded_once() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(85),
        0,
        "direct JMP successor lookahead"
    );
}

#[test]
fn admitted_jcc_fallthrough_successor_is_decoded_once() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(228),
        0,
        "JCC fall-through successor lookahead"
    );
}

#[test]
fn smc_during_successor_lookahead_rejects_then_rebuilds() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(229),
        0,
        "SMC invalidates the whole transaction containing a decoded successor"
    );
}
