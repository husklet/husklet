use super::*;

fn sequencer() -> Sequencer {
    Sequencer::spawn(RemoteCommandSink::new(
        "/tmp/hl-gl-transport-test-unused.sock",
    ))
    .expect("actor")
}

fn record(log: &Arc<Mutex<Vec<u8>>>, value: u8) -> Plan<()> {
    let log = Arc::clone(log);
    Plan::new(move |_| {
        log.lock().expect("log").push(value);
        Ok(())
    })
}

#[test]
fn plans_execute_in_serial_order() {
    let sequencer = sequencer();
    let log = Arc::new(Mutex::new(Vec::new()));
    let first = sequencer.submit(record(&log, 1)).expect("first");
    let second = sequencer.submit(record(&log, 2)).expect("second");

    assert_eq!(first.serial().get(), 1);
    assert_eq!(second.serial().get(), 2);
    first.wait().expect("first result");
    second.wait().expect("second result");
    assert_eq!(*log.lock().expect("log"), [1, 2]);
}

#[test]
fn concurrent_serials_match_fifo_execution() {
    let sequencer = sequencer();
    let log = Arc::new(Mutex::new(Vec::new()));
    let submissions = (0..32u8)
        .map(|value| {
            let sequencer = sequencer.clone();
            let log = Arc::clone(&log);
            thread::spawn(move || {
                let ticket = sequencer.submit(record(&log, value)).expect("submission");
                let serial = ticket.serial().get();
                ticket.wait().expect("result");
                (serial, value)
            })
        })
        .collect::<Vec<_>>();
    let mut submitted = submissions
        .into_iter()
        .map(|submission| submission.join().expect("producer"))
        .collect::<Vec<_>>();
    submitted.sort_unstable_by_key(|(serial, _)| *serial);
    let expected = submitted
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();

    assert_eq!(*log.lock().expect("log"), expected);
}

#[test]
fn compound_plan_does_not_interleave() {
    let sequencer = sequencer();
    let log = Arc::new(Mutex::new(Vec::new()));
    let compound_log = Arc::clone(&log);
    let compound = Plan::new(move |_| {
        let mut log = compound_log.lock().expect("log");
        log.push(1);
        log.push(2);
        Ok(())
    });

    let first = sequencer.submit(compound).expect("compound");
    let second = sequencer.submit(record(&log, 3)).expect("following");
    first.wait().expect("compound result");
    second.wait().expect("following result");
    assert_eq!(*log.lock().expect("log"), [1, 2, 3]);
}

#[test]
fn typed_output_reaches_ticket() {
    let sequencer = sequencer();
    let ticket = sequencer
        .submit(Plan::new(|_| Ok(String::from("capabilities"))))
        .expect("typed plan");

    assert_eq!(ticket.wait().expect("typed result"), "capabilities");
}

#[test]
fn failure_does_not_stop_actor() {
    let sequencer = sequencer();
    let failed = Plan::new(|_| Err::<(), _>(GpuError::Decode("expected failure".into())));

    assert!(sequencer
        .submit(failed)
        .expect("failed plan accepted")
        .wait()
        .is_err());
    assert_eq!(
        sequencer
            .submit(Plan::new(|_| Ok(7)))
            .expect("following plan")
            .wait()
            .expect("actor survived"),
        7
    );
}

#[test]
fn shutdown_drains_and_rejects_new_plans() {
    let sequencer = sequencer();
    let log = Arc::new(Mutex::new(Vec::new()));
    let accepted = sequencer.submit(record(&log, 1)).expect("accepted");

    assert_eq!(sequencer.shutdown(), Shutdown::Stopped);

    accepted.wait().expect("accepted plan drained");
    assert_eq!(
        sequencer.submit(Plan::new(|_| Ok(()))).err(),
        Some(SubmitError::Closed)
    );
    assert_eq!(*log.lock().expect("log"), [1]);
}

#[test]
fn actor_thread_shutdown_does_not_wait_for_itself() {
    let sequencer = sequencer();
    let actor = sequencer.clone();
    let ticket = sequencer
        .submit(Plan::new(move |_| {
            actor.shutdown();
            Ok(7)
        }))
        .expect("reentrant plan");

    assert_eq!(ticket.wait().expect("reentrant result"), 7);
    assert_eq!(
        sequencer.submit(Plan::new(|_| Ok(()))).err(),
        Some(SubmitError::Closed)
    );
}

#[test]
fn dropping_last_sequencer_on_actor_thread_does_not_self_join() {
    let sequencer = sequencer();
    let actor = sequencer.clone();
    let ticket = sequencer
        .submit(Plan::new(move |_| {
            drop(actor);
            Ok(11)
        }))
        .expect("drop plan");
    drop(sequencer);

    assert_eq!(ticket.wait().expect("drop result"), 11);
}

#[test]
fn shutdown_is_bounded_while_a_plan_is_blocked() {
    let sequencer = sequencer();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let ticket = sequencer
        .submit(Plan::new(move |_| {
            started_tx.send(()).expect("started");
            release_rx.recv().expect("released");
            Ok(())
        }))
        .expect("blocking plan");
    started_rx.recv().expect("plan started");

    let started = std::time::Instant::now();
    assert_eq!(sequencer.shutdown(), Shutdown::Detached);
    assert_eq!(sequencer.shutdown(), Shutdown::Detached);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        sequencer.submit(Plan::new(|_| Ok(()))).err(),
        Some(SubmitError::Closed)
    );

    release_tx.send(()).expect("release actor");
    ticket.wait().expect("blocked plan completed");
}

#[test]
fn ticket_deadline_does_not_wait_for_a_blocked_operation() {
    let sequencer = sequencer();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let ticket = sequencer
        .submit(Plan::new(move |_| {
            started_tx.send(()).expect("started");
            release_rx.recv().expect("released");
            Ok(())
        }))
        .expect("blocking plan");
    started_rx.recv().expect("plan started");

    let started = std::time::Instant::now();
    assert!(matches!(
        ticket.wait_for(Duration::from_millis(10)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    release_tx.send(()).expect("release actor");
}
