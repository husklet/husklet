use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt::Write as _};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ScenarioKey {
    pub scenario: String,
    pub target: String,
    pub image_digest: String,
    pub engine_archive_hash: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    RuntimeFail,
    MaterializationFail,
    ArchSkip,
    InfrastructureFail,
    Timeout,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioOutcome {
    pub key: ScenarioKey,
    pub category: String,
    pub declared_image: String,
    pub resolved_digest: Option<String>,
    pub step: serde_json::Value,
    pub timeout_seconds: u64,
    pub checks: Vec<String>,
    pub started_at: String,
    pub duration_ms: u64,
    pub status: Status,
    pub process_exit: Option<i32>,
    pub process_signal: Option<i32>,
    pub expected_failure: bool,
    pub error: Option<String>,
    pub log_path: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub key: ScenarioKey,
    pub started_at: String,
    pub log_path: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct WorkflowKey {
    pub workflow: String,
    pub engine_archive_hash: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowAttempt {
    pub key: WorkflowKey,
    pub started_at: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowOutcome {
    pub key: WorkflowKey,
    pub started_at: String,
    pub duration_ms: u64,
    pub status: Status,
    pub process_exit: Option<i32>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BatchMetadata {
    pub run_id: String,
    pub started_unix_ms: u64,
    pub engine_archive_hash: String,
    pub targets: Vec<String>,
    pub images: BTreeMap<String, String>,
    pub categories: Vec<String>,
    pub filters: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BatchReport {
    pub metadata: BatchMetadata,
    pub outcomes: Vec<ScenarioOutcome>,
    #[serde(default)]
    pub workflows: Vec<WorkflowOutcome>,
    #[serde(default)]
    pub scenario_cases: u64,
    #[serde(default)]
    pub workflow_cases: u64,
    #[serde(default)]
    pub runtime_cases: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub timed_out: u64,
}
impl BatchReport {
    pub fn new(
        mut metadata: BatchMetadata,
        mut outcomes: Vec<ScenarioOutcome>,
        mut workflows: Vec<WorkflowOutcome>,
    ) -> Self {
        outcomes.sort_by(|a, b| a.key.cmp(&b.key));
        workflows.sort_by(|a, b| a.key.cmp(&b.key));
        metadata
            .categories
            .extend(outcomes.iter().map(|outcome| outcome.category.clone()));
        metadata.categories.sort();
        metadata.categories.dedup();
        let scenario_cases = outcomes.len().try_into().unwrap_or(u64::MAX);
        let workflow_cases = workflows.len().try_into().unwrap_or(u64::MAX);
        let mut value = Self {
            metadata,
            outcomes,
            workflows,
            scenario_cases,
            workflow_cases,
            runtime_cases: scenario_cases.saturating_add(workflow_cases),
            passed: 0,
            failed: 0,
            skipped: 0,
            timed_out: 0,
        };
        for item in &value.outcomes {
            match item.status {
                Status::Pass => value.passed += 1,
                Status::ArchSkip => value.skipped += 1,
                Status::Timeout => value.timed_out += 1,
                Status::RuntimeFail | Status::MaterializationFail | Status::InfrastructureFail => {
                    value.failed += 1;
                }
            }
        }
        for item in &value.workflows {
            match item.status {
                Status::Pass => value.passed += 1,
                Status::ArchSkip => value.skipped += 1,
                Status::Timeout => value.timed_out += 1,
                Status::RuntimeFail | Status::MaterializationFail | Status::InfrastructureFail => {
                    value.failed += 1;
                }
            }
        }
        value
    }
    pub fn markdown(&self) -> String {
        let mut text = format!(
            "# Scenario batch `{}`\n\n| Passed | Failed | Skipped | Timed out |\n|---:|---:|---:|---:|\n| {} | {} | {} | {} |\n\n",
            self.metadata.run_id, self.passed, self.failed, self.skipped, self.timed_out
        );
        for item in &self.outcomes {
            writeln!(
                text,
                "- `{}` `{}` {:?} ({} ms)\n",
                item.key.scenario, item.key.target, item.status, item.duration_ms
            )
            .expect("writing to a String cannot fail");
        }
        for item in &self.workflows {
            writeln!(
                text,
                "- workflow `{}` {:?} ({} ms)\n",
                item.key.workflow, item.status, item.duration_ms
            )
            .expect("writing to a String cannot fail");
        }
        text
    }
}
