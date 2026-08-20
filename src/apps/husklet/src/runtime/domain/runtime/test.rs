use super::{PrimaryLifecycle, PrimaryStartError, Runtime};
use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;

struct Primary {
    starts: Mutex<VecDeque<Result<(), PrimaryStartError>>>,
    discards: Mutex<Vec<Result<(), String>>>,
    /// What each successful restoring start settles into, oldest first. An empty queue answers
    /// "the restored container stayed alive".
    settles: Mutex<VecDeque<Option<String>>>,
}

impl Primary {
    fn new(starts: impl IntoIterator<Item = Result<(), &'static str>>, discard: Result<(), &'static str>) -> Self {
        Self {
            starts: Mutex::new(
                starts
                    .into_iter()
                    .map(|result| result.map_err(|error| PrimaryStartError::Process(error.to_owned())))
                    .collect(),
            ),
            discards: Mutex::new(vec![discard.map_err(str::to_owned)]),
            settles: Mutex::new(VecDeque::new()),
        }
    }

    fn settling(mut self, settles: impl IntoIterator<Item = Option<&'static str>>) -> Self {
        self.settles = Mutex::new(settles.into_iter().map(|value| value.map(str::to_owned)).collect());
        self
    }
}

impl PrimaryLifecycle for Primary {
    async fn start_primary(&self) -> Result<(), PrimaryStartError> {
        self.starts.lock().unwrap().pop_front().expect("expected start")
    }

    async fn discard_primary_checkpoint(&self) -> Result<(), String> {
        self.discards.lock().unwrap().pop().expect("expected discard")
    }

    async fn restored_primary_failure(&self) -> io::Result<Option<String>> {
        Ok(self.settles.lock().unwrap().pop_front().flatten())
    }
}

#[tokio::test]
async fn corrupt_repository_start_failure_remains_a_global_startup_error() {
    let primary = Primary {
        starts: Mutex::new(VecDeque::from([Err(PrimaryStartError::Repository(io::Error::other(
            "container catalog corrupt",
        )))])),
        discards: Mutex::new(vec![Ok(())]),
        settles: Mutex::new(VecDeque::new()),
    };

    let error = Runtime::start_primary(&primary, true).await.unwrap_err();

    assert!(error.to_string().contains("container catalog corrupt"));
    assert_eq!(primary.discards.lock().unwrap().len(), 1);
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

/// A restore that launches its engine and then dies rebuilding guest memory must be reported as a
/// failure to resume and replaced by a fresh container. Believing the launch leaves the execution
/// domain serving a container that is already `Exited`, and every terminal opened against it
/// conflicts with the dead process instead of giving the user a shell.
#[tokio::test]
async fn a_restore_that_dies_after_launching_is_replaced_by_a_fresh_container() {
    let primary = Primary::new([Ok(()), Ok(())], Ok(())).settling([Some("the restored container exited immediately")]);

    let failures = Runtime::start_primary(&primary, true).await.unwrap();

    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("checkpoint restore failed"));
    assert!(failures[0].contains("the restored container exited immediately"));
    assert!(failures[0].contains("started a fresh primary process"));
    assert!(primary.starts.lock().unwrap().is_empty());
    assert!(primary.discards.lock().unwrap().is_empty());
}

/// The replacement is only attempted for a start that actually restored a checkpoint, and a
/// restored container that stays alive is not disturbed.
#[tokio::test]
async fn a_restored_container_that_stays_alive_is_left_alone() {
    let primary = Primary::new([Ok(())], Ok(())).settling([None]);

    assert!(Runtime::start_primary(&primary, true).await.unwrap().is_empty());
    assert_eq!(primary.discards.lock().unwrap().len(), 1);

    let plain = Primary::new([Ok(())], Ok(())).settling([Some("must not be consulted")]);

    assert!(Runtime::start_primary(&plain, false).await.unwrap().is_empty());
    assert_eq!(plain.settles.lock().unwrap().len(), 1);
}
