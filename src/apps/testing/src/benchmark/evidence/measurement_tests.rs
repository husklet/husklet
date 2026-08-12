use super::Measurement;
use std::{
    cell::Cell,
    fs::OpenOptions,
    time::{Duration, Instant},
};

#[test]
fn quiet_is_rechecked_while_the_box_lock_is_held() {
    let directory = tempfile::tempdir().unwrap();
    let intent = directory.path().join("wanted");
    let box_lock = directory.path().join("box");
    let probes = Cell::new(0);
    let result = Measurement::acquire_with(
        &intent,
        &box_lock,
        Duration::ZERO,
        Duration::from_secs(1),
        |lock_held| {
            probes.set(probes.get() + 1);
            if probes.get() == 1 {
                assert!(!lock_held);
                return Ok(true);
            }
            assert!(lock_held);
            let competing = OpenOptions::new().read(true).write(true).open(&box_lock).unwrap();
            assert!(fs2::FileExt::try_lock_shared(&competing).is_err());
            Ok(false)
        },
    );
    let Err(error) = result else {
        panic!("measurement accepted a busy post-lock probe");
    };
    assert!(error.to_string().contains("became busy"));
    assert_eq!(probes.get(), 2);
}

#[test]
fn acquisition_timeout_is_one_deadline_across_quiet_and_box_lock() {
    let directory = tempfile::tempdir().unwrap();
    let intent = directory.path().join("wanted");
    let box_path = directory.path().join("box");
    let competing = super::open_lock(&box_path).unwrap();
    fs2::FileExt::lock_shared(&competing).unwrap();
    let started = Instant::now();
    let result = Measurement::acquire_with(&intent, &box_path, Duration::ZERO, Duration::from_millis(200), |_| {
        std::thread::sleep(Duration::from_millis(150));
        Ok(true)
    });
    let Err(error) = result else {
        panic!("measurement acquired a lock held by a competitor");
    };
    assert!(error.to_string().contains("timed out acquiring"));
    assert!(started.elapsed() < Duration::from_millis(300));
}

#[cfg(target_os = "linux")]
#[test]
fn holder_count_observes_the_box_descriptor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("box");
    let held = super::open_lock(&path).unwrap();
    assert_eq!(super::box_lock_holder_count(&path).unwrap(), 1);
    fs2::FileExt::lock_shared(&held).unwrap();
    assert_eq!(super::box_lock_holder_count(&path).unwrap(), 1);
    drop(held);
    assert_eq!(super::box_lock_holder_count(&path).unwrap(), 0);
}
