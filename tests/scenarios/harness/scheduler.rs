//! Resource-aware coordinator for the complete compatibility catalog.

mod cache;
mod process;
mod queue;
mod report;
mod workflow;

use crate::contract::Target;
use cache::RunLock;
use process::ProcessGroup;
use queue::{owns, requirements, Resources, Task, TASKS};
use std::{
    collections::VecDeque,
    env, fs,
    path::Path,
    process::Stdio,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, sync::Mutex, task::JoinSet};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) struct Options {
    jobs: usize,
    category: Option<String>,
    case: Option<String>,
    resume: bool,
    offline: bool,
    dry_run: bool,
    target: Target,
}

impl Options {
    pub(crate) fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, Error> {
        let mut value = Self {
            jobs: std::thread::available_parallelism()
                .map_or(4, usize::from)
                .min(8),
            category: None,
            case: None,
            resume: false,
            offline: false,
            dry_run: false,
            target: Target::Arm64,
        };
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--jobs" => value.jobs = arguments.next().ok_or("--jobs needs a value")?.parse()?,
                "--category" => {
                    value.category = Some(arguments.next().ok_or("--category needs a value")?);
                }
                "--case" => value.case = Some(arguments.next().ok_or("--case needs a value")?),
                "--resume" => value.resume = true,
                "--offline" => value.offline = true,
                "--prefetch" => value.offline = false,
                "--dry-run" => value.dry_run = true,
                "--target" => {
                    value.target = match arguments.next().as_deref() {
                        Some("arm64") => Target::Arm64,
                        Some("amd64") => Target::Amd64,
                        Some(value) => {
                            return Err(format!("unsupported target {value:?}").into());
                        }
                        None => return Err("--target needs a value".into()),
                    };
                }
                other => return Err(format!("unknown all-suite option {other:?}").into()),
            }
        }
        if value.jobs == 0 {
            return Err("--jobs must be greater than zero".into());
        }
        Ok(value)
    }

    fn report(&self, tasks: &VecDeque<Task>) {
        let selected = crate::registry::build()
            .scenarios()
            .filter(|scenario| {
                tasks.iter().any(|task| owns(task, scenario.id))
                    && self.case.as_deref().is_none_or(|id| id == scenario.id)
            })
            .count();
        let workflows = usize::from(self.category.is_none() && self.case.is_none())
            * crate::workflows::NAMES.len();
        println!(
            "{}",
            serde_json::json!({
                "scenario_contracts": selected,
                "scenario_categories": tasks.len(),
                "workflows": workflows,
                "runtime_cases": selected + workflows,
                "jobs": self.jobs,
            })
        );
    }

    fn finish(&self, run: &str, failures: &[&str]) -> Result<(), Error> {
        report::finish(self, run)?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("failed categories: {}", failures.join(", ")).into())
        }
    }

    async fn workflows(
        &self,
        executable: &Path,
        cache: &Path,
        run: &str,
    ) -> Result<Vec<&'static str>, Error> {
        workflow::run(executable, self.jobs, cache, run, self.resume, self.target).await
    }
}

pub(crate) async fn run(options: Options) -> Result<(), Error> {
    let executable = env::current_exe()?;
    let cache = cache::absolute(options.target)?;
    let run = env::var("HL_SCENARIO_RUN_ID")
        .unwrap_or_else(|_| default_run_id(SystemTime::now(), std::process::id()));
    let workers = cache
        .parent()
        .unwrap_or_else(|| Path::new("target/scenarios"))
        .join("workers")
        .join(&run);
    if !options.resume && workers.exists() {
        fs::remove_dir_all(&workers)?;
    }
    fs::create_dir_all(&workers)?;
    let tasks = TASKS
        .iter()
        .copied()
        .filter(|task| {
            options
                .category
                .as_deref()
                .is_none_or(|name| task.category == name)
                && options.case.as_deref().is_none_or(|id| owns(task, id))
        })
        .collect::<VecDeque<_>>();
    if tasks.is_empty() {
        return Err("category filter selected no compatibility group".into());
    }
    if options.dry_run {
        options.report(&tasks);
        return Ok(());
    }
    let _run_lock = RunLock::acquire(&cache)?;
    let queue = Arc::new(Mutex::new(tasks));
    let resources = Resources::new();
    let offline = options.offline;
    let target = options.target;
    let mut workers_set = JoinSet::new();
    for worker in 0..options.jobs {
        let queue = queue.clone();
        let resources = resources.clone();
        let executable = executable.clone();
        let cache = cache.clone();
        let case = options.case.clone();
        let run = run.clone();
        workers_set.spawn(async move {
            let mut failures = Vec::new();
            loop {
                let Some(task) = queue.lock().await.pop_front() else {
                    break;
                };
                let mut required = requirements(&task, case.as_deref());
                if !offline {
                    required.insert(crate::contract::Resource::Registry);
                }
                let mut permits = Vec::new();
                for resource in required {
                    if let Some(permit) = resources.acquire(resource).await? {
                        permits.push(permit);
                    }
                }
                let worker_cache = cache.clone();
                eprintln!("START [{}] {}", worker + 1, task.category);
                let mut child = Command::new(&executable);
                child
                    .arg(task.command)
                    .env("HL_SCENARIO_RUN_ID", &run)
                    .env("HL_SCENARIO_CHILD", "1")
                    .env("HL_SCENARIO_IMAGE_CACHE", &worker_cache)
                    .env("HL_SCENARIO_TARGET", target.name())
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                if let Some(case) = &case {
                    child.env("HL_SCENARIO_CASE", case);
                }
                if offline {
                    child.env("HL_SCENARIO_OFFLINE", "1");
                }
                let status = ProcessGroup::spawn(&mut child)?.wait().await?;
                eprintln!("DONE  [{}] {} {status}", worker + 1, task.category);
                if !status.success() {
                    failures.push(task.category);
                }
            }
            Ok::<_, Error>(failures)
        });
    }
    let mut failures = Vec::new();
    while let Some(result) = workers_set.join_next().await {
        failures.extend(result??);
    }
    if options.category.is_none() && options.case.is_none() {
        failures.extend(options.workflows(&executable, &cache, &run).await?);
    }
    options.finish(&run, &failures)
}

fn default_run_id(started: SystemTime, process: u32) -> String {
    let epoch = started
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("all-{epoch}-{process}")
}

pub(crate) fn test_requirements() -> Result<(), Error> {
    queue::test_requirements()
}

pub(crate) fn test_run_lock() -> Result<(), Error> {
    cache::test_lock()
}

pub(crate) fn test_workflow_target_cache() {
    workflow::test_target_cache();
}

pub(crate) mod tests {
    use super::{default_run_id, process, Options};
    use std::time::{Duration, UNIX_EPOCH};

    pub(crate) fn run_ids_survive_process_id_reuse() {
        let process = 40_634;
        let first = default_run_id(UNIX_EPOCH + Duration::from_secs(1), process);
        let second = default_run_id(UNIX_EPOCH + Duration::from_secs(2), process);
        assert_ne!(first, second);
    }

    pub(crate) fn options_reject_zero_jobs_and_accept_filters() {
        assert!(Options::parse(["--jobs".into(), "0".into()].into_iter()).is_err());
        let options = Options::parse(
            [
                "--jobs".into(),
                "3".into(),
                "--category".into(),
                "terminal".into(),
                "--dry-run".into(),
                "--target".into(),
                "amd64".into(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(options.jobs, 3);
        assert_eq!(options.category.as_deref(), Some("terminal"));
        assert!(options.dry_run);
        assert_eq!(options.target, crate::contract::Target::Amd64);
    }

    pub(crate) async fn timeout_reaps_owned_process_group() {
        process::test_timeout_reaps_owned_process_group()
            .await
            .unwrap();
    }
}
