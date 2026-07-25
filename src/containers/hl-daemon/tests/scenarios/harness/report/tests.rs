use std::{collections::BTreeMap, fs};

use crate::contract::Scenario;

use super::{
    Attempt, BatchMetadata, BatchReport, ScenarioAttempt, ScenarioBatch, ScenarioKey,
    ScenarioOutcome, Status, Store, WorkflowAttempt, WorkflowKey, WorkflowOutcome,
};

fn outcome(id: &str) -> ScenarioOutcome {
    ScenarioOutcome {
        key: ScenarioKey {
            scenario: id.into(),
            target: "arm64".into(),
            image_digest: "sha256:image".into(),
            engine_archive_hash: "sha256:engine".into(),
        },
        category: "database".into(),
        declared_image: "postgres:17".into(),
        resolved_digest: Some("sha256:image".into()),
        step: serde_json::json!({"exec":"select 1"}),
        timeout_seconds: 30,
        checks: vec!["ready".into()],
        started_at: "1970-01-01T00:00:00Z".into(),
        duration_ms: 7,
        status: Status::Pass,
        process_exit: Some(0),
        process_signal: None,
        expected_failure: false,
        error: None,
        log_path: format!("logs/{id}.log"),
    }
}
pub(crate) fn persistence_resume_and_summaries_are_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::create(temp.path(), "run-1").unwrap();
    store
        .begin(&Attempt {
            key: outcome("a").key,
            started_at: "1".into(),
            log_path: "logs/a.log".into(),
        })
        .unwrap();
    store.append(&outcome("b")).unwrap();
    store.append(&outcome("a")).unwrap();
    let before = fs::read_to_string(store.root().join("results.jsonl")).unwrap();
    assert_eq!(
        store
            .invalidate(&["a".into()], "sha256:engine", "stale fixture")
            .unwrap(),
        1
    );
    let corrected = fs::read_to_string(store.root().join("results.jsonl")).unwrap();
    assert!(corrected.starts_with(&before));
    assert_eq!(corrected.lines().count(), 3);
    fs::write(store.root().join("results.tmp"), b"{partial").unwrap();
    let resumed = store.resume().unwrap();
    assert_eq!(resumed.len(), 2);
    assert_eq!(
        resumed[&outcome("a").key].status,
        Status::InfrastructureFail
    );
    assert_eq!(
        fs::read_to_string(store.root().join("attempts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(resumed.contains_key(&outcome("a").key));
    let mut stale = outcome("a").key;
    stale.engine_archive_hash = "sha256:stale".into();
    assert!(!resumed.contains_key(&stale));
    assert!(store.root().ends_with("run-1"));
    assert!(store
        .log_path("db/postgres:17")
        .ends_with("logs/db_postgres_17.log"));
    let metadata = BatchMetadata {
        run_id: "run-1".into(),
        started_unix_ms: 1,
        engine_archive_hash: "sha256:engine".into(),
        targets: vec!["arm64".into()],
        images: BTreeMap::from([("postgres:17".into(), "sha256:image".into())]),
        categories: vec!["database".into()],
        filters: vec![],
    };
    let report = BatchReport::new(metadata, resumed.into_values().collect(), Vec::new());
    assert_eq!(report.outcomes[0].key.scenario, "a");
    store.finish(&report).unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join("latest")).unwrap(),
        "run-1"
    );
    assert!(fs::read_to_string(temp.path().join("run-1/summary.md"))
        .unwrap()
        .contains("Passed"));
}

pub(crate) fn workflow_evidence_is_append_only_resumable_and_summarized() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::create(temp.path(), "workflows").unwrap();
    let key = WorkflowKey {
        workflow: "docker-build".into(),
        engine_archive_hash: "sha256:engine".into(),
    };
    store
        .begin_workflow(&WorkflowAttempt {
            key: key.clone(),
            started_at: "1".into(),
        })
        .unwrap();
    let failed = WorkflowOutcome {
        key: key.clone(),
        started_at: "1".into(),
        duration_ms: 2,
        status: Status::RuntimeFail,
        process_exit: Some(1),
        error: Some("exit status: 1".into()),
    };
    store.append_workflow(&failed).unwrap();
    let mut passed = failed;
    passed.duration_ms = 3;
    passed.status = Status::Pass;
    passed.process_exit = Some(0);
    passed.error = None;
    store.append_workflow(&passed).unwrap();

    assert_eq!(
        fs::read_to_string(store.root().join("workflow-attempts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(store.root().join("workflow-results.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let resumed = store.resume_workflows().unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[&key], passed);
    let report = BatchReport::new(
        BatchMetadata {
            run_id: "workflows".into(),
            started_unix_ms: 1,
            engine_archive_hash: "sha256:engine".into(),
            targets: vec!["arm64".into()],
            images: BTreeMap::new(),
            categories: vec!["docker-build".into()],
            filters: Vec::new(),
        },
        vec![outcome("scenario")],
        resumed.into_values().collect(),
    );
    assert_eq!(report.scenario_cases, 1);
    assert_eq!(report.workflow_cases, 1);
    assert_eq!(report.runtime_cases, 2);
    assert_eq!(report.passed, 2);
    store.finish(&report).unwrap();
    let summary: BatchReport =
        serde_json::from_slice(&fs::read(store.root().join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary.workflows, vec![passed]);
    assert_eq!(summary.runtime_cases, 2);
}

pub(crate) fn legacy_resume_filters_before_case_body() {
    let value = outcome("a");
    let batch = ScenarioBatch {
        category: "test".into(),
        archive: "sha256:engine".into(),
        store: None,
        recorded: BTreeMap::from([(value.key.clone(), value)]),
    };
    let scenario = Scenario::new("a", "sha256:image");
    let mut invoked = 0;
    if batch.begin(&scenario).unwrap().is_some() {
        invoked += 1;
    }
    assert_eq!(invoked, 0);
}

pub(crate) fn concurrent_category_writers_preserve_every_result() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().to_owned();
    let workers = (0..8)
        .map(|worker| {
            let base = base.clone();
            std::thread::spawn(move || {
                let store = Store::create(&base, "parallel").unwrap();
                for case in 0..32 {
                    let id = format!("worker-{worker}/case-{case}");
                    let value = outcome(&id);
                    store
                        .begin(&Attempt {
                            key: value.key.clone(),
                            started_at: "1".into(),
                            log_path: value.log_path.clone(),
                        })
                        .unwrap();
                    store.append(&value).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let store = Store::create(&base, "parallel").unwrap();
    assert_eq!(store.resume().unwrap().len(), 8 * 32);
    assert_eq!(
        fs::read_to_string(store.root().join("attempts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        8 * 32
    );
}

pub(crate) fn architecture_skip_writes_exactly_one_terminal_result() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::create(temp.path(), "single-terminal").unwrap();
    let results = store.root().join("results.jsonl");
    let mut batch = ScenarioBatch {
        category: "copy".into(),
        archive: "sha256:engine".into(),
        store: Some(store),
        recorded: BTreeMap::new(),
    };
    let scenario = Scenario::new("cpcoherence/example.amd", "alpine:3.20");
    let attempt: ScenarioAttempt = batch.begin(&scenario).unwrap().unwrap();
    batch.skip(&scenario, attempt).unwrap();
    let raw = fs::read_to_string(results).unwrap();
    assert_eq!(raw.lines().count(), 1);
    let terminal: ScenarioOutcome = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(terminal.key.scenario, scenario.id);
    assert_eq!(terminal.status, Status::ArchSkip);
    assert!(batch.begin(&scenario).unwrap().is_none());
}
