use super::{PrimaryLifecycle, Runtime};
use std::collections::VecDeque;
use std::sync::Mutex;

struct Primary {
    starts: Mutex<VecDeque<Result<(), String>>>,
    discards: Mutex<Vec<Result<(), String>>>,
}

impl Primary {
    fn new(starts: impl IntoIterator<Item = Result<(), &'static str>>, discard: Result<(), &'static str>) -> Self {
        Self {
            starts: Mutex::new(starts.into_iter().map(|result| result.map_err(str::to_owned)).collect()),
            discards: Mutex::new(vec![discard.map_err(str::to_owned)]),
        }
    }
}

impl PrimaryLifecycle for Primary {
    async fn start_primary(&self) -> Result<(), String> {
        self.starts.lock().unwrap().pop_front().expect("expected start")
    }

    async fn discard_primary_checkpoint(&self) -> Result<(), String> {
        self.discards.lock().unwrap().pop().expect("expected discard")
    }
}

#[tokio::test]
async fn failed_primary_restore_discards_checkpoint_and_starts_fresh() {
    let primary = Primary::new([Err("invalid image"), Ok(())], Ok(()));

    let failures = Runtime::start_primary(&primary, true).await.unwrap();

    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("invalid image"));
    assert!(failures[0].contains("started a fresh primary process"));
    assert!(primary.starts.lock().unwrap().is_empty());
    assert!(primary.discards.lock().unwrap().is_empty());
}

#[tokio::test]
async fn process_local_fresh_start_failure_is_reported_without_aborting_domain_startup() {
    let primary = Primary::new([Err("checkpoint unavailable"), Err("volume unavailable")], Ok(()));

    let failures = Runtime::start_primary(&primary, true).await.unwrap();

    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("checkpoint unavailable"));
    assert!(failures[0].contains("volume unavailable"));
    assert!(primary.starts.lock().unwrap().is_empty());
    assert!(primary.discards.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ordinary_process_spawn_failure_does_not_discard_unrelated_state() {
    let primary = Primary::new([Err("network unavailable")], Ok(()));

    let failures = Runtime::start_primary(&primary, false).await.unwrap();

    assert_eq!(failures, ["workspace: start failed: network unavailable"]);
    assert_eq!(primary.discards.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn durable_checkpoint_update_failure_remains_a_global_startup_error() {
    let primary = Primary::new([Err("checkpoint unavailable")], Err("repository read-only"));

    let error = Runtime::start_primary(&primary, true).await.unwrap_err();

    assert!(error.to_string().contains("discard workspace checkpoint"));
    assert!(error.to_string().contains("repository read-only"));
}
