use std::{
    collections::BTreeMap,
    io,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::contract::{Scenario, Target};

use super::{
    schema::{Attempt, BatchMetadata, BatchReport, ScenarioKey, ScenarioOutcome, Status},
    store::Store,
};

pub struct LegacyBatch {
    pub(super) category: String,
    pub(super) archive: String,
    pub(super) store: Option<Store>,
    pub(super) recorded: BTreeMap<ScenarioKey, ScenarioOutcome>,
}
pub struct LegacyAttempt {
    key: ScenarioKey,
    started: Duration,
    timer: Instant,
}
impl LegacyBatch {
    pub fn new(category: &str) -> io::Result<Self> {
        let store = Store::from_env()?;
        let recorded = store
            .as_ref()
            .map(Store::resume)
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            category: category.into(),
            archive: std::env::var("HL_ENGINE_ARCHIVE_SHA256").unwrap_or_else(|_| "unknown".into()),
            store,
            recorded,
        })
    }
    pub fn begin(&self, scenario: &Scenario) -> io::Result<Option<LegacyAttempt>> {
        let key = ScenarioKey {
            scenario: scenario.id.into(),
            target: "arm64".into(),
            image_digest: scenario.image.into(),
            engine_archive_hash: self.archive.clone(),
        };
        if self
            .recorded
            .get(&key)
            .is_some_and(|value| value.status != Status::InfrastructureFail)
        {
            return Ok(None);
        }
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        if let Some(store) = &self.store {
            store.begin(&Attempt {
                key: key.clone(),
                started_at: started.as_millis().to_string(),
                log_path: store.log_path(scenario.id).display().to_string(),
            })?;
        }
        Ok(Some(LegacyAttempt {
            key,
            started,
            timer: Instant::now(),
        }))
    }
    pub fn complete(
        &mut self,
        scenario: &Scenario,
        attempt: LegacyAttempt,
        result: &Result<(), Box<dyn std::error::Error>>,
    ) -> io::Result<()> {
        let error = result.as_ref().err().map(ToString::to_string);
        let status = if result.is_ok() {
            Status::Pass
        } else if error
            .as_deref()
            .is_some_and(|value| value.contains("materialization"))
        {
            Status::MaterializationFail
        } else if error
            .as_deref()
            .is_some_and(|value| value.contains("timed out"))
        {
            Status::Timeout
        } else {
            Status::RuntimeFail
        };
        self.record(scenario, attempt, status, error)
    }

    fn record(
        &mut self,
        scenario: &Scenario,
        attempt: LegacyAttempt,
        status: Status,
        error: Option<String>,
    ) -> io::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let outcome = ScenarioOutcome {
            key: attempt.key,
            category: self.category.clone(),
            declared_image: scenario.image.into(),
            resolved_digest: None,
            step: serde_json::to_value(&scenario.step).unwrap_or_default(),
            timeout_seconds: scenario.timeout_seconds,
            checks: scenario
                .checks
                .iter()
                .map(|value| format!("{value:?}"))
                .collect(),
            started_at: attempt.started.as_millis().to_string(),
            duration_ms: attempt
                .timer
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            status,
            process_exit: None,
            process_signal: None,
            expected_failure: scenario.expected_failures.contains(&Target::Arm64),
            error,
            log_path: store.log_path(scenario.id).display().to_string(),
        };
        store.append(&outcome)?;
        Store::write_log(
            Path::new(&outcome.log_path),
            outcome.error.as_deref().unwrap_or("pass\n"),
        )?;
        self.recorded.insert(outcome.key.clone(), outcome);
        Ok(())
    }
    pub fn skip(&mut self, scenario: &Scenario, attempt: LegacyAttempt) -> io::Result<()> {
        self.record(scenario, attempt, Status::ArchSkip, None)
    }
    pub fn finish(self, filters: Vec<String>) -> io::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store.finish(&BatchReport::new(
            BatchMetadata {
                run_id: std::env::var("HL_SCENARIO_RUN_ID").unwrap_or_default(),
                started_unix_ms: 0,
                engine_archive_hash: self.archive,
                targets: vec!["arm64".into()],
                images: BTreeMap::new(),
                categories: vec![self.category],
                filters,
            },
            self.recorded.into_values().collect(),
            Vec::new(),
        ))
    }
}
