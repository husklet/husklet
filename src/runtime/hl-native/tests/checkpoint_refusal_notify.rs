#![cfg(all(feature = "native-test-hooks", unix))]

#[test]
fn refusal_notification_does_not_wait_for_a_reply() {
    for isa in [1, 2] {
        hl_native::checkpoint_channel_notify_test(isa, 0).expect("one-way notification waited for a broker reply");
    }
}

#[test]
fn refusal_notification_does_not_wait_for_socket_capacity() {
    for isa in [1, 2] {
        hl_native::checkpoint_channel_notify_test(isa, 1).expect("one-way notification blocked on a full channel");
    }
}

#[test]
fn refusal_notification_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_channel_notify_test(isa, 2), Err(-22));
    }
    assert_eq!(hl_native::checkpoint_channel_notify_test(9, 0), Err(-22));
}
