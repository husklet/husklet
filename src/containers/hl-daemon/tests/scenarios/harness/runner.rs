//! Shared executor for typed exact-image compatibility contracts.

use crate::{
    contract::{Group, Scenario, Target},
    report::{Attempt, BatchMetadata, BatchReport, ScenarioKey, ScenarioOutcome, Status, Store},
};
use hl_container::Containers;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[path = "runner/evidence.rs"]
mod evidence;
#[path = "runner/execution.rs"]
mod execution;
#[path = "runner/resources.rs"]
mod resources;

pub(crate) use evidence::test_service_diagnostics;
pub(crate) use resources::test_resources;

type Error = Box<dyn std::error::Error>;
const MATERIALIZE_TIMEOUT: Duration = Duration::from_secs(300);
const SERVICE_LOG_LIMIT: u64 = 64 * 1024;

#[derive(Default)]
struct Summary {
    passed: usize,
    skipped: usize,
    xfailed: usize,
    xpassed: usize,
    failed: Vec<String>,
}

pub(crate) struct Runner<'a> {
    containers: &'a Containers,
    target: Target,
}

impl<'a> Runner<'a> {
    pub(crate) fn arm64(containers: &'a Containers) -> Self {
        Self {
            containers,
            target: Target::Arm64,
        }
    }

    pub(crate) async fn run(&self, group: Group) -> Result<(), Error> {
        let store = Store::from_env()?;
        let archive =
            std::env::var("HL_ENGINE_ARCHIVE_SHA256").unwrap_or_else(|_| "unknown".into());
        let mut recorded = store
            .as_ref()
            .map(Store::resume)
            .transpose()?
            .unwrap_or_default();
        let selected = std::env::var("HL_SCENARIO_CASE")
            .or_else(|_| std::env::var("HL_DATABASE_CASE"))
            .ok();
        let cases = group
            .scenarios
            .iter()
            .filter(|case| selected.as_deref().is_none_or(|id| case.id == id))
            .collect::<Vec<_>>();
        if let Some(id) = &selected {
            if cases.is_empty() {
                return Err(format!("scenario {id:?} is not in group {}", group.name).into());
            }
        }
        let total = cases.len();
        let mut summary = Summary::default();
        for case in cases {
            self.run_case(case, &archive, store.as_ref(), &mut recorded, &mut summary)
                .await?;
        }
        println!(
            "{} scenarios: {} pass; {} skip; {} xfail; {} xpass; {} fail; {} total",
            group.name,
            summary.passed,
            summary.skipped,
            summary.xfailed,
            summary.xpassed,
            summary.failed.len().saturating_sub(summary.xpassed),
            total
        );
        if let Some(store) = &store {
            let metadata = BatchMetadata {
                run_id: std::env::var("HL_SCENARIO_RUN_ID").unwrap_or_default(),
                started_unix_ms: 0,
                engine_archive_hash: archive,
                targets: vec![format!("{:?}", self.target).to_lowercase()],
                images: BTreeMap::new(),
                categories: vec![group.name.into()],
                filters: selected.into_iter().collect(),
            };
            store.finish(&BatchReport::new(
                metadata,
                recorded.into_values().collect(),
                Vec::new(),
            ))?;
        }
        if summary.failed.is_empty() {
            Ok(())
        } else {
            Err(summary.failed.join("\n").into())
        }
    }

    async fn run_case(
        &self,
        case: &Scenario,
        archive: &str,
        store: Option<&Store>,
        recorded: &mut BTreeMap<ScenarioKey, ScenarioOutcome>,
        summary: &mut Summary,
    ) -> Result<(), Error> {
        let key = ScenarioKey {
            scenario: case.id.into(),
            target: format!("{:?}", self.target).to_lowercase(),
            image_digest: case.image.into(),
            engine_archive_hash: archive.into(),
        };
        if let Some(outcome) = recorded.get(&key) {
            if resume_outcome(case, self.target, outcome, summary) {
                println!("RESUME {} {:?}", case.id, outcome.status);
                return Ok(());
            }
        }
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let timer = Instant::now();
        if let Some(store) = store {
            store.begin(&Attempt {
                key: key.clone(),
                started_at: started.as_millis().to_string(),
                log_path: store.log_path(case.id).display().to_string(),
            })?;
        }
        if !case.targets.contains(&self.target) {
            println!("SKIP {}: does not target {:?}", case.id, self.target);
            summary.skipped += 1;
            if let Some(store) = store {
                let outcome = evidence::report_outcome(
                    case,
                    key,
                    Status::ArchSkip,
                    None,
                    timer.elapsed(),
                    started,
                    store,
                );
                store.append(&outcome)?;
                Store::write_log(Path::new(&outcome.log_path), "architecture skip\n")?;
                recorded.insert(outcome.key.clone(), outcome);
            }
            return Ok(());
        }
        let expected = case.expected_failures.contains(&self.target);
        let result = self.execute(case).await;
        let report_error = result.as_ref().err().map(ToString::to_string);
        let status = match &result {
            Ok(()) => Status::Pass,
            Err(error) if error.to_string().contains("materialization") => {
                Status::MaterializationFail
            }
            Err(error) if error.to_string().contains("timed out") => Status::Timeout,
            Err(_) => Status::RuntimeFail,
        };
        if let Some(store) = store {
            let outcome = evidence::report_outcome(
                case,
                key,
                status,
                report_error,
                timer.elapsed(),
                started,
                store,
            );
            store.append(&outcome)?;
            Store::write_log(
                Path::new(&outcome.log_path),
                outcome.error.as_deref().unwrap_or("pass\n"),
            )?;
            recorded.insert(outcome.key.clone(), outcome);
        }
        match result {
            Ok(()) if expected => {
                println!("XPASS {}", case.id);
                summary.xpassed += 1;
                summary.failed.push(format!("{}: unexpected pass", case.id));
            }
            Ok(()) => {
                println!("PASS {}", case.id);
                summary.passed += 1;
            }
            Err(error) if expected => {
                println!("XFAIL {}: {error}", case.id);
                summary.xfailed += 1;
            }
            Err(error) => {
                println!("FAIL {}: {error}", case.id);
                summary.failed.push(format!("{}: {error}", case.id));
            }
        }
        Ok(())
    }
}

fn resume_outcome(
    case: &Scenario,
    target: Target,
    outcome: &ScenarioOutcome,
    summary: &mut Summary,
) -> bool {
    let expected = case.expected_failures.contains(&target);
    match outcome.status {
        Status::InfrastructureFail => return false,
        Status::Pass if expected => {
            summary.xpassed += 1;
            summary.failed.push(format!("{}: unexpected pass", case.id));
        }
        Status::Pass => summary.passed += 1,
        Status::ArchSkip => summary.skipped += 1,
        Status::RuntimeFail | Status::MaterializationFail | Status::Timeout if expected => {
            summary.xfailed += 1;
        }
        Status::RuntimeFail | Status::MaterializationFail | Status::Timeout => {
            let error = outcome
                .error
                .as_deref()
                .unwrap_or("recorded scenario failure");
            summary.failed.push(format!("{}: {error}", case.id));
        }
    }
    true
}

fn test_resume_outcomes() {
    fn recorded(status: Status, error: Option<&str>) -> ScenarioOutcome {
        ScenarioOutcome {
            key: ScenarioKey {
                scenario: "resume/example".into(),
                target: "arm64".into(),
                image_digest: "alpine:3.20".into(),
                engine_archive_hash: "sha256:engine".into(),
            },
            category: "resume".into(),
            declared_image: "alpine:3.20".into(),
            resolved_digest: None,
            step: serde_json::Value::Null,
            timeout_seconds: 30,
            checks: Vec::new(),
            started_at: "0".into(),
            duration_ms: 1,
            status,
            process_exit: None,
            process_signal: None,
            expected_failure: false,
            error: error.map(str::to_owned),
            log_path: "resume.log".into(),
        }
    }

    for status in [
        Status::RuntimeFail,
        Status::MaterializationFail,
        Status::Timeout,
    ] {
        let case = Scenario::new("resume/example", "alpine:3.20");
        let mut summary = Summary::default();
        assert!(resume_outcome(
            &case,
            Target::Arm64,
            &recorded(status, Some("recorded failure")),
            &mut summary
        ));
        assert_eq!(summary.failed, ["resume/example: recorded failure"]);
    }

    let case = Scenario::new("resume/example", "alpine:3.20");
    let mut summary = Summary::default();
    assert!(!resume_outcome(
        &case,
        Target::Arm64,
        &recorded(Status::InfrastructureFail, Some("retry me")),
        &mut summary
    ));
    assert!(summary.failed.is_empty());

    let expected = Scenario::new("resume/example", "alpine:3.20").xfail(&[Target::Arm64]);
    let mut summary = Summary::default();
    assert!(resume_outcome(
        &expected,
        Target::Arm64,
        &recorded(Status::RuntimeFail, Some("known failure")),
        &mut summary
    ));
    assert_eq!(summary.xfailed, 1);
    assert!(summary.failed.is_empty());

    let mut summary = Summary::default();
    assert!(resume_outcome(
        &expected,
        Target::Arm64,
        &recorded(Status::Pass, None),
        &mut summary
    ));
    assert_eq!(summary.xpassed, 1);
    assert_eq!(summary.failed, ["resume/example: unexpected pass"]);
}
