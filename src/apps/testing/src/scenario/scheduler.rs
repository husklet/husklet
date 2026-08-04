use super::{
    Options,
    definition::{Resource, Scenario},
    execution, ledger,
};
use crate::suite::{Error, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};

pub(super) struct Summary {
    pub passed: usize,
    pub expected_failures: usize,
    pub failed: Vec<String>,
}

pub(super) async fn run(scenarios: Vec<Scenario>, options: &Options, report: &Path) -> Result<Summary, Error> {
    let work = plan(scenarios, options)?;
    if work.is_empty() {
        return Err("no scenario cases support the selected target(s)".into());
    }
    let stamp = fingerprint(&work).await?;
    let keys = work.iter().map(|item| item.key.clone()).collect();
    let report = report.to_path_buf();
    let resume = options.resume;
    let opened = tokio::task::spawn_blocking(move || {
        ledger::Ledger::open(&report, &stamp, &keys, resume).map_err(|error| error.to_string())
    })
    .await??;
    let ledger = Arc::new(opened.ledger);
    let prior = opened.prior;
    let semaphore = Arc::new(Semaphore::new(options.jobs));
    let resources = Arc::new(ResourcePool::new());
    let mut running = JoinSet::new();
    for item in work.into_iter().filter(|item| !prior.contains_key(&item.key)) {
        spawn(&mut running, item, Arc::clone(&semaphore), Arc::clone(&resources));
    }
    let mut completed = drain(&mut running, &ledger).await?;
    completed.sort_by(|left, right| left.key.cmp(&right.key));
    let summary = summarize(&prior, completed);
    tokio::task::spawn_blocking(move || ledger.finish().map_err(|error| error.to_string())).await??;
    Ok(summary)
}

fn spawn(
    running: &mut JoinSet<Result<Completed, String>>,
    work: Work,
    semaphore: Arc<Semaphore>,
    resources: Arc<ResourcePool>,
) {
    running.spawn(async move {
        let (_resources, _permit) = resources.admit(&work.resources, semaphore).await?;
        let started = std::time::Instant::now();
        let result = execution::run_case(work.scenario, work.case_index, work.target).await;
        Ok(Completed {
            key: work.key,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            result,
        })
    });
}

async fn enter(semaphore: Arc<Semaphore>) -> Result<OwnedSemaphorePermit, String> {
    semaphore
        .acquire_owned()
        .await
        .map_err(|_| "scenario worker pool closed".to_owned())
}

async fn drain(
    running: &mut JoinSet<Result<Completed, String>>,
    ledger: &Arc<ledger::Ledger>,
) -> Result<Vec<Completed>, Error> {
    let mut completed = Vec::new();
    while let Some(result) = running.join_next().await {
        let result = result?.map_err(|error| -> Error { error.into() })?;
        let row = result.row();
        let recording = Arc::clone(ledger);
        tokio::task::spawn_blocking(move || recording.record(row).map_err(|error| error.to_string())).await??;
        completed.push(result);
    }
    Ok(completed)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct WorkKey {
    pub id: String,
    pub target: Target,
}

struct Work {
    key: WorkKey,
    scenario: Arc<Scenario>,
    case_index: usize,
    target: Target,
    resources: Vec<Resource>,
}

struct Completed {
    key: WorkKey,
    elapsed_ms: u64,
    result: execution::CaseResult,
}

impl Completed {
    fn row(&self) -> ledger::Row {
        let (status, diagnostic) = self.result.evidence();
        ledger::Row {
            key: self.key.clone(),
            status,
            elapsed_ms: self.elapsed_ms,
            diagnostic,
        }
    }
}

fn plan(scenarios: Vec<Scenario>, options: &Options) -> Result<Vec<Work>, Error> {
    let mut work = Vec::new();
    let mut keys = BTreeSet::new();
    for scenario in scenarios {
        let scenario = Arc::new(scenario);
        for target in options.targets() {
            for (case_index, case) in scenario.cases.iter().enumerate() {
                if case.supports(target) {
                    let key = WorkKey {
                        id: case.id.clone(),
                        target,
                    };
                    if !keys.insert(key.clone()) {
                        return Err(format!("duplicate scenario case/target key {} {}", case.id, target.name()).into());
                    }
                    work.push(Work {
                        key,
                        scenario: Arc::clone(&scenario),
                        case_index,
                        target,
                        resources: case.resources.clone(),
                    });
                }
            }
        }
    }
    work.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(work)
}

struct ResourcePool {
    permits: BTreeMap<Resource, Arc<Semaphore>>,
}

impl ResourcePool {
    fn new() -> Self {
        let permits = [
            (Resource::DiskHeavy, 2),
            (Resource::Registry, 2),
            (Resource::Network, 4),
            (Resource::HostPort, 4),
            (Resource::ImageMutation, 1),
            (Resource::ProcessHeavy, 1),
        ]
        .into_iter()
        .map(|(resource, capacity)| (resource, Arc::new(Semaphore::new(capacity))))
        .collect();
        Self { permits }
    }

    async fn acquire(&self, requested: &[Resource]) -> Result<Vec<OwnedSemaphorePermit>, String> {
        let mut requested = requested.to_vec();
        requested.sort_unstable();
        let mut permits = Vec::with_capacity(requested.len());
        for resource in requested {
            let Some(semaphore) = self.permits.get(&resource) else {
                continue;
            };
            permits.push(
                Arc::clone(semaphore)
                    .acquire_owned()
                    .await
                    .map_err(|_| "scenario resource pool closed".to_owned())?,
            );
        }
        Ok(permits)
    }

    async fn admit(
        &self,
        requested: &[Resource],
        jobs: Arc<Semaphore>,
    ) -> Result<(Vec<OwnedSemaphorePermit>, OwnedSemaphorePermit), String> {
        let resources = self.acquire(requested).await?;
        let job = enter(jobs).await?;
        Ok((resources, job))
    }
}

async fn fingerprint(work: &[Work]) -> Result<String, Error> {
    let mut inputs = Vec::new();
    for item in work {
        let case = &item.scenario.cases[item.case_index];
        inputs.push(item.scenario.definition.clone());
        inputs.extend(case.fixtures.iter().map(|fixture| fixture.source.clone()));
        inputs.extend(case.stdout_contains.iter().cloned());
        inputs.extend(case.stdout_exact.iter().cloned());
    }
    inputs.sort();
    inputs.dedup();
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        for path in inputs {
            digest.update(path.to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(std::fs::read(path).map_err(|error| error.to_string())?);
        }
        Ok::<_, String>(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
    })
    .await?
    .map_err(Into::into)
}

fn summarize(prior: &BTreeMap<WorkKey, ledger::Row>, completed: Vec<Completed>) -> Summary {
    let mut summary = Summary {
        passed: 0,
        expected_failures: 0,
        failed: Vec::new(),
    };
    for row in prior.values() {
        println!(
            "RESUME {} {} {} elapsed_ms={}",
            row.status,
            row.key.id,
            row.key.target.name(),
            row.elapsed_ms
        );
        count(row.status, &row.key, &row.diagnostic, &mut summary);
    }
    for item in completed {
        let (status, diagnostic) = item.result.evidence();
        println!(
            "{} {} {} elapsed_ms={}: {}",
            status.to_uppercase(),
            item.key.id,
            item.key.target.name(),
            item.elapsed_ms,
            diagnostic
        );
        count(status, &item.key, &diagnostic, &mut summary);
    }
    summary
}

fn count(status: &str, key: &WorkKey, diagnostic: &str, summary: &mut Summary) {
    match status {
        "pass" => summary.passed += 1,
        "xfail" => summary.expected_failures += 1,
        _ => summary
            .failed
            .push(format!("{} {}: {diagnostic}", key.id, key.target.name())),
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourcePool, enter};
    use crate::scenario::definition::Resource;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        sync::{Barrier, Semaphore},
        task::JoinSet,
    };

    #[tokio::test]
    async fn one_pool_bounds_all_scenario_work() {
        let semaphore = Arc::new(Semaphore::new(3));
        let barrier = Arc::new(Barrier::new(3));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut tasks = JoinSet::new();
        for _ in 0..12 {
            let semaphore = Arc::clone(&semaphore);
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            tasks.spawn(async move {
                let _permit = enter(semaphore).await.unwrap();
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                barrier.wait().await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
        let mut completed = 0;
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
            completed += 1;
        }
        assert_eq!(completed, 12);
        assert_eq!(maximum.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exclusive_resources_serialize_and_pty_does_not() {
        let pool = Arc::new(ResourcePool::new());
        let first = pool.acquire(&[Resource::ProcessHeavy]).await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                pool.acquire(&[Resource::ProcessHeavy])
            )
            .await
            .is_err()
        );
        assert!(pool.acquire(&[Resource::Pty]).await.unwrap().is_empty());
        drop(first);
        assert!(pool.acquire(&[Resource::ProcessHeavy]).await.is_ok());
    }

    #[tokio::test]
    async fn unrelated_resources_do_not_collide() {
        let pool = ResourcePool::new();
        let _mutation = pool.acquire(&[Resource::ImageMutation]).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), pool.acquire(&[Resource::Network]))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn resource_waiters_do_not_occupy_job_slots() {
        let pool = Arc::new(ResourcePool::new());
        let jobs = Arc::new(Semaphore::new(1));
        let held = pool.acquire(&[Resource::ProcessHeavy]).await.unwrap();
        let waiting_pool = Arc::clone(&pool);
        let waiting_jobs = Arc::clone(&jobs);
        let waiting = tokio::spawn(async move { waiting_pool.admit(&[Resource::ProcessHeavy], waiting_jobs).await });
        tokio::task::yield_now().await;
        let unrelated = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            pool.admit(&[Resource::Network], jobs),
        )
        .await;
        assert!(unrelated.is_ok());
        waiting.abort();
        drop(held);
    }
}
