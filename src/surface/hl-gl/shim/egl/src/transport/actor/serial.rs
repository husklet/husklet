use super::*;

#[test]
fn maximum_serial_is_issued_before_exhaustion() {
    let sequencer = Sequencer::spawn(RemoteCommandSink::new(
        "/tmp/hl-gl-transport-serial-test-unused.sock",
    ))
    .expect("actor");
    sequencer.actor.sequence.lock().expect("sequence").next = Some(u64::MAX);

    let last = sequencer
        .submit(Plan::new(|_| Ok(())))
        .expect("last serial");
    assert_eq!(last.serial().get(), u64::MAX);
    last.wait().expect("last plan");
    assert_eq!(
        sequencer.submit(Plan::new(|_| Ok(()))).err(),
        Some(SubmitError::SerialExhausted)
    );
}
