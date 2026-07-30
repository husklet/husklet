use super::*;

#[test]
fn failed_state_precedes_panicking_ticket_wakeup() {
    let sequencer = Sequencer::spawn(RemoteCommandSink::new(
        "/tmp/hl-gl-transport-panic-test-unused.sock",
    ))
    .expect("actor");
    let (started_tx, started_rx) = mpsc::channel();
    let (panic_tx, panic_rx) = mpsc::channel();
    let panicking = sequencer
        .submit(Plan::new(move |_| -> hl_gpu::Result<()> {
            started_tx.send(()).expect("started");
            panic_rx.recv().expect("panic released");
            panic!("expected transport plan panic");
        }))
        .expect("panicking plan");
    started_rx.recv().expect("plan started");
    let queued = sequencer
        .submit(Plan::new(|_| Ok(17)))
        .expect("queued plan");
    panic_tx.send(()).expect("release panic");

    let current = panicking.wait().expect_err("current plan must fail");
    assert!(current.to_string().contains("transport plan panicked"));
    assert_eq!(
        sequencer.submit(Plan::new(|_| Ok(()))).err(),
        Some(SubmitError::ActorFailed)
    );
    let queued = queued.wait().expect_err("queued plan must fail");
    assert!(queued
        .to_string()
        .contains("stopped before completing a plan"));
}
