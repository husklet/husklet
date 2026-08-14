#![cfg(feature = "native-test-hooks")]

fn run(isa: u32, scenario: u32) -> (i64, u32, u64) {
    hl_native::bound_vector_io_test(isa, scenario).expect("native bound-vector test hook")
}

#[test]
fn production_bound_vector_io_obeys_partial_and_no_issue_contracts() {
    for isa in [1, 2] {
        assert_eq!(run(isa, 0), (4096, 1, 4096));
        assert_eq!(run(isa, 2), (4096, 1, 4096));
        assert_eq!(run(isa, 1), (-14, 0, 0));
        assert_eq!(run(isa, 3), (-9, 0, 0));
    }
}
