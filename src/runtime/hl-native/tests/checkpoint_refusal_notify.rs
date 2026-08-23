#![cfg(all(feature = "native-test-hooks", unix))]

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn refusal_notification_does_not_wait_for_a_reply() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        hl_native::checkpoint_channel_notify_test(isa, 0).expect("one-way notification waited for a broker reply");
    }
}

#[test]
fn refusal_notification_does_not_wait_for_socket_capacity() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        hl_native::checkpoint_channel_notify_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} one-way notification postcondition failed at {status}"));
    }
}

#[test]
fn refusal_notification_hook_rejects_unknown_scenarios() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_channel_notify_test(isa, 2), Err(-22));
    }
    assert_eq!(hl_native::checkpoint_channel_notify_test(9, 0), Err(-22));
}
