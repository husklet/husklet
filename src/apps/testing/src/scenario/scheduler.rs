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
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};

mod process_lifecycle;

pub(super) struct Summary {
    pub passed: usize,
    pub expected_failures: usize,
    pub skipped: usize,
    pub failed: Vec<String>,
}

pub(super) fn inventory(scenarios: Vec<Scenario>, options: &Options) -> Result<Vec<WorkKey>, Error> {
    plan(scenarios, options).map(|work| work.into_iter().flat_map(|item| item.keys).collect())
}

pub(super) async fn run(scenarios: Vec<Scenario>, options: &Options, report: &Path) -> Result<Summary, Error> {
    let work = plan(scenarios, options)?;
    if work.is_empty() {
        return Err("no scenario cases support the selected target(s)".into());
    }
    let stamp = fingerprint(&work, options.warm_provider).await?;
    let keys = work.iter().flat_map(|item| item.keys.iter().cloned()).collect();
    let report = report.to_path_buf();
    let resume = options.selection.resume;
    let opened = tokio::task::spawn_blocking(move || {
        ledger::Ledger::open(&report, &stamp, &keys, resume).map_err(|error| error.to_string())
    })
    .await??;
    let ledger = Arc::new(opened.ledger);
    let prior = opened.prior;
    let completed_keys = Arc::new(prior.keys().cloned().collect::<BTreeSet<_>>());
    let semaphore = Arc::new(Semaphore::new(options.selection.jobs));
    let resources = Arc::new(ResourcePool::new());
    let providers = options
        .warm_provider
        .then(|| Arc::new(ProviderPool::new(options.selection.jobs)));
    let mut running = JoinSet::new();
    for item in work
        .into_iter()
        .filter(|item| item.keys.iter().any(|key| !completed_keys.contains(key)))
    {
        spawn(
            &mut running,
            item,
            Arc::clone(&semaphore),
            Arc::clone(&resources),
            providers.clone(),
            Arc::clone(&completed_keys),
        );
    }
    let mut completed = drain(&mut running, &ledger).await?;
    completed.sort_by(|left, right| left.key.cmp(&right.key));
    let summary = summarize(&prior, completed);
    tokio::task::spawn_blocking(move || ledger.finish().map_err(|error| error.to_string())).await??;
    Ok(summary)
}

fn spawn(
    running: &mut JoinSet<Result<Vec<Completion>, String>>,
    work: Work,
    semaphore: Arc<Semaphore>,
    resources: Arc<ResourcePool>,
    providers: Option<Arc<ProviderPool>>,
    prior: Arc<BTreeSet<WorkKey>>,
) {
    running.spawn(async move {
        let (_resources, _permit) = resources.admit(&work.resources, semaphore).await?;
        let provider_slot = providers.as_ref().map(|pool| pool.next_slot());
        let local = if providers.is_none() {
            Some(execution::Provider::start().await.map_err(|error| error.to_string())?)
        } else {
            None
        };
        let first_sample = work.keys.first().map_or(1, |key| key.sample);
        for _ in 0..work.warmups {
            let warmup = run_on_provider(
                providers.as_deref(),
                provider_slot,
                local.as_ref(),
                Arc::clone(&work.scenario),
                work.case_index,
                work.target,
                first_sample,
            )
            .await;
            if !matches!(warmup.result, execution::CaseResult::Passed) {
                return Ok(work
                    .keys
                    .into_iter()
                    .filter(|key| !prior.contains(key))
                    .map(|key| Completion {
                        key,
                        elapsed_ms: 0,
                        result: execution::CaseResult::Failed("scenario warmup did not pass".to_owned()),
                        timing: warmup.timing.clone(),
                    })
                    .collect());
            }
        }
        let mut completed = Vec::new();
        let mut stopped = false;
        for key in work.keys.into_iter().filter(|key| !prior.contains(key)) {
            if stopped {
                completed.push(Completion {
                    key,
                    elapsed_ms: 0,
                    result: execution::CaseResult::NotRun("not run after preceding sample failed".to_owned()),
                    timing: execution::PhaseTiming::default(),
                });
                continue;
            }
            let started = std::time::Instant::now();
            let outcome = run_on_provider(
                providers.as_deref(),
                provider_slot,
                local.as_ref(),
                Arc::clone(&work.scenario),
                work.case_index,
                work.target,
                key.sample,
            )
            .await;
            stopped = !matches!(outcome.result, execution::CaseResult::Passed);
            completed.push(Completion {
                key,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                result: outcome.result,
                timing: outcome.timing,
            });
        }
        drop(local);
        Ok(completed)
    });
}

async fn run_on_provider(
    pool: Option<&ProviderPool>,
    slot: Option<usize>,
    local: Option<&execution::Provider>,
    scenario: Arc<Scenario>,
    case_index: usize,
    target: Target,
    sample: u16,
) -> execution::CaseOutcome {
    match (pool, slot, local) {
        (Some(pool), Some(slot), _) => pool.run(slot, scenario, case_index, target, sample).await,
        (_, _, Some(provider)) => execution::run_case_on(provider, scenario, case_index, target, sample).await,
        _ => execution::CaseOutcome {
            result: execution::CaseResult::Failed("scenario provider is unavailable".to_owned()),
            timing: execution::PhaseTiming::default(),
        },
    }
}

struct ProviderPool {
    slots: Vec<AsyncMutex<ProviderState<execution::Provider>>>,
    next: std::sync::atomic::AtomicUsize,
}

impl ProviderPool {
    fn new(jobs: usize) -> Self {
        Self {
            slots: (0..jobs).map(|_| AsyncMutex::new(ProviderState::default())).collect(),
            next: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn next_slot(&self) -> usize {
        use std::sync::atomic::Ordering;
        self.next.fetch_add(1, Ordering::Relaxed) % self.slots.len()
    }

    async fn run(
        &self,
        index: usize,
        scenario: Arc<Scenario>,
        case_index: usize,
        target: Target,
        sample: u16,
    ) -> execution::CaseOutcome {
        let mut slot = self.slots[index].lock().await;
        let mut provider_setup_us = 0;
        if slot.value.is_none() {
            let started = std::time::Instant::now();
            match execution::Provider::start().await {
                Ok(provider) => {
                    provider_setup_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                    slot.value = Some(provider);
                }
                Err(error) => {
                    return execution::CaseOutcome {
                        result: execution::CaseResult::Failed(error.to_string()),
                        timing: execution::PhaseTiming::default(),
                    };
                }
            }
        }
        let Some(provider) = slot.value.as_ref() else {
            return execution::CaseOutcome {
                result: execution::CaseResult::Failed("provider slot was not initialized".to_owned()),
                timing: execution::PhaseTiming::default(),
            };
        };
        let mut outcome = execution::run_case_on(provider, scenario, case_index, target, sample).await;
        outcome.timing.setup_us = outcome.timing.setup_us.saturating_add(provider_setup_us);
        slot.finish(&outcome.result);
        outcome
    }
}

struct ProviderState<T> {
    value: Option<T>,
}

impl<T> Default for ProviderState<T> {
    fn default() -> Self {
        Self { value: None }
    }
}

impl<T> ProviderState<T> {
    fn finish(&mut self, result: &execution::CaseResult) {
        if !retains_provider(result) {
            self.value = None;
        }
    }
}

fn retains_provider(result: &execution::CaseResult) -> bool {
    matches!(result, execution::CaseResult::Passed)
}

async fn enter(semaphore: Arc<Semaphore>) -> Result<OwnedSemaphorePermit, String> {
    semaphore
        .acquire_owned()
        .await
        .map_err(|_| "scenario worker pool closed".to_owned())
}

async fn drain(
    running: &mut JoinSet<Result<Vec<Completion>, String>>,
    ledger: &Arc<ledger::Ledger>,
) -> Result<Vec<Completion>, Error> {
    let mut completed = Vec::new();
    while let Some(result) = running.join_next().await {
        let results = result?.map_err(|error| -> Error { error.into() })?;
        for result in results {
            let row = result.row();
            let recording = Arc::clone(ledger);
            tokio::task::spawn_blocking(move || recording.record(row).map_err(|error| error.to_string())).await??;
            completed.push(result);
        }
    }
    Ok(completed)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct WorkKey {
    pub id: String,
    pub target: Target,
    pub sample: u16,
}

struct Work {
    keys: Vec<WorkKey>,
    scenario: Arc<Scenario>,
    case_index: usize,
    target: Target,
    resources: Vec<Resource>,
    warmups: u16,
}

struct Completion {
    key: WorkKey,
    elapsed_ms: u64,
    result: execution::CaseResult,
    timing: execution::PhaseTiming,
}

impl Completion {
    fn row(&self) -> ledger::Row {
        let (status, diagnostic) = self.result.evidence();
        ledger::Row {
            attempt: crate::journal::Attempt {
                key: self.key.clone(),
                status,
                elapsed_ms: self.elapsed_ms,
            },
            timing: self.timing.clone(),
            diagnostic,
        }
    }
}

fn plan(scenarios: Vec<Scenario>, options: &Options) -> Result<Vec<Work>, Error> {
    let mut work = Vec::new();
    let mut keys = BTreeSet::new();
    for scenario in scenarios {
        let scenario = Arc::new(scenario);
        for target in options.selection.targets() {
            for (case_index, case) in scenario.cases.iter().enumerate() {
                if case.supports(target) && selected_class(options.class, case.class) {
                    let mut samples = Vec::with_capacity(usize::from(case.repetitions));
                    for sample in 1..=case.repetitions {
                        let key = WorkKey {
                            id: case.id.clone(),
                            target,
                            sample,
                        };
                        if !keys.insert(key.clone()) {
                            return Err(format!(
                                "duplicate scenario case/target/sample key {} {} {sample}",
                                case.id,
                                target.name()
                            )
                            .into());
                        }
                        samples.push(key);
                    }
                    work.push(Work {
                        keys: samples,
                        scenario: Arc::clone(&scenario),
                        case_index,
                        target,
                        resources: case.resources.clone(),
                        warmups: case.warmups,
                    });
                }
            }
        }
    }
    work.sort_by(|left, right| left.keys.cmp(&right.keys));
    Ok(work)
}

fn selected_class(selected: Option<super::definition::Class>, case: super::definition::Class) -> bool {
    selected.is_none_or(|class| class == case)
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

async fn fingerprint(work: &[Work], warm_provider: bool) -> Result<String, Error> {
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
        digest.update(if warm_provider {
            b"warm-provider".as_slice()
        } else {
            b"isolated-provider".as_slice()
        });
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

fn summarize(prior: &BTreeMap<WorkKey, ledger::Row>, completed: Vec<Completion>) -> Summary {
    let mut summary = Summary {
        passed: 0,
        expected_failures: 0,
        skipped: 0,
        failed: Vec::new(),
    };
    for row in prior.values() {
        println!(
            "RESUME {} {} {} sample={} elapsed_ms={} setup_us={} execution_us={} payload_us={} teardown_us={}",
            row.attempt.status,
            row.attempt.key.id,
            row.attempt.key.target.name(),
            row.attempt.key.sample,
            row.attempt.elapsed_ms,
            row.timing.setup_us,
            row.timing.execution_us,
            row.timing
                .payload_us
                .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
            row.timing.teardown_us
        );
        count(row.attempt.status, &row.attempt.key, &row.diagnostic, &mut summary);
    }
    for item in completed {
        let (status, diagnostic) = item.result.evidence();
        println!(
            "{} {} {} sample={} elapsed_ms={} setup_us={} execution_us={} payload_us={} teardown_us={}: {}",
            status.to_uppercase(),
            item.key.id,
            item.key.target.name(),
            item.key.sample,
            item.elapsed_ms,
            item.timing.setup_us,
            item.timing.execution_us,
            item.timing
                .payload_us
                .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
            item.timing.teardown_us,
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
        "skip" => summary.skipped += 1,
        _ => summary
            .failed
            .push(format!("{} {}: {diagnostic}", key.id, key.target.name())),
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderState, ResourcePool, Summary, WorkKey, count, enter, retains_provider, selected_class};
    use crate::scenario::definition::{Class, Resource};
    use crate::scenario::execution::CaseResult;
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
    async fn process_heavy_serializes_while_light_and_pty_do_not() {
        let pool = Arc::new(ResourcePool::new());
        let promptly = std::time::Duration::from_millis(10);
        let first = pool.acquire(&[Resource::ProcessHeavy]).await.unwrap();
        assert!(
            tokio::time::timeout(promptly, pool.acquire(&[Resource::ProcessHeavy]))
                .await
                .is_err()
        );
        let light = tokio::time::timeout(promptly, pool.acquire(&[]))
            .await
            .unwrap()
            .unwrap();
        assert!(light.is_empty());
        let pty = tokio::time::timeout(promptly, pool.acquire(&[Resource::Pty]))
            .await
            .unwrap()
            .unwrap();
        assert!(pty.is_empty());
        drop(first);
        let released = tokio::time::timeout(promptly, pool.acquire(&[Resource::ProcessHeavy]))
            .await
            .unwrap();
        assert!(released.is_ok());
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

    #[test]
    fn class_filter_preserves_the_fast_suite_boundary() {
        assert!(selected_class(None, Class::Quick));
        assert!(selected_class(Some(Class::Quick), Class::Quick));
        assert!(!selected_class(Some(Class::Quick), Class::Long));
        assert!(selected_class(Some(Class::Long), Class::Long));
    }

    #[test]
    fn only_a_passed_case_can_reuse_provider_state() {
        assert!(retains_provider(&CaseResult::Passed));
        assert!(!retains_provider(&CaseResult::Failed("failure".to_owned())));
        assert!(!retains_provider(&CaseResult::ExpectedFailure("expected".to_owned())));
        assert!(!retains_provider(&CaseResult::UnexpectedPass));
        assert!(!retains_provider(&CaseResult::NotRun("not run".to_owned())));
    }

    #[test]
    fn fake_provider_identity_is_stable_then_evicted_without_state_leakage() {
        #[derive(Debug, Eq, PartialEq)]
        struct FakeProvider {
            identity: u64,
            case_state: Vec<&'static str>,
        }

        let mut slot = ProviderState::default();
        slot.value = Some(FakeProvider {
            identity: 41,
            case_state: Vec::new(),
        });
        let warmup_identity = slot.value.as_ref().unwrap().identity;
        slot.value.as_mut().unwrap().case_state.push("warmup");
        slot.finish(&CaseResult::Passed);
        let first_sample_identity = slot.value.as_ref().unwrap().identity;
        slot.value.as_mut().unwrap().case_state.push("sample-1");
        slot.finish(&CaseResult::Passed);
        let second_sample_identity = slot.value.as_ref().unwrap().identity;
        assert_eq!(
            (warmup_identity, first_sample_identity, second_sample_identity),
            (41, 41, 41)
        );
        slot.finish(&CaseResult::UnexpectedPass);
        assert!(slot.value.is_none(), "non-pass retained provider state");

        slot.value = Some(FakeProvider {
            identity: 42,
            case_state: Vec::new(),
        });
        assert_eq!(slot.value.as_ref().unwrap().identity, 42);
        assert!(
            slot.value.as_ref().unwrap().case_state.is_empty(),
            "next case observed prior state"
        );
    }

    #[test]
    fn skipped_samples_are_counted_without_becoming_failures() {
        let mut summary = Summary {
            passed: 0,
            expected_failures: 0,
            skipped: 0,
            failed: Vec::new(),
        };
        count(
            "skip",
            &WorkKey {
                id: "sample/skipped".to_owned(),
                target: crate::suite::Target::Arm64,
                sample: 2,
            },
            "preceding sample failed",
            &mut summary,
        );
        assert_eq!(summary.skipped, 1);
        assert!(summary.failed.is_empty());
    }
}
