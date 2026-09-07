#[test]
fn xmm_destination_is_canonical_before_every_indirect_terminal() {
    let families = [
        (239, "register JMP"),
        (240, "RIP-memory JMP"),
        (241, "register CALL"),
        (242, "memory JMP"),
        (243, "RIP-memory CALL"),
        (244, "memory CALL"),
        (245, "RET"),
    ];
    for (scenario, family) in families {
        assert_eq!(
            hl_native::x86_64_translit_displaced_test(scenario),
            0,
            "SSE destination must be committed before {family} miss, hit, and IRQ paths"
        );
    }
}

#[test]
fn signal_capture_keeps_canonical_xmm_across_indirect_terminal_windows() {
    assert_eq!(hl_native::x86_64_translit_displaced_test(149), 0, "memory JMP stages");
    assert_eq!(hl_native::x86_64_translit_displaced_test(226), 0, "memory CALL stages");
}
