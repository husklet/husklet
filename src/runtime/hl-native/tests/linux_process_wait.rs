#![cfg(all(feature = "native-test-hooks", target_os = "linux"))]

#[test]
fn simultaneous_infinite_waiters_share_one_blocking_reap_and_close_afterward() {
    hl_native::linux_process_wait_test(1).expect("two waiters must share one retained exit result");
}

#[test]
fn destroy_kills_and_joins_an_infinite_blocking_waiter() {
    hl_native::linux_process_wait_test(2).expect("destroy must release its blocking process waiter");
}
