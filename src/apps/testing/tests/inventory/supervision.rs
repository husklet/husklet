#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const STALL_FLOOR: Duration = Duration::from_secs(60);
const TERM_GRACE: Duration = Duration::from_secs(2);
const REAP_GRACE: Duration = Duration::from_secs(2);
const OUTPUT_LIMIT: u64 = 1024 * 1024;
const ERROR_LIMIT: u64 = 64 * 1024;
#[cfg(target_os = "linux")]
const PROCESS_LIMIT: usize = 4096;
#[cfg(target_os = "linux")]
const PROCESS_SAMPLE_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Debug)]
pub(super) enum Outcome {
    Exited(ExitStatus),
    Interrupted(i32),
    OutputLimit,
    Stalled,
    TimedOut(TimeoutEvidence),
}

#[derive(Debug)]
pub(super) struct TimeoutEvidence {
    elapsed: Duration,
    tree_ticks: Option<u64>,
    host_busy_percent: Option<u8>,
    runnable: Option<usize>,
    cpus: usize,
    class: TimeoutClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimeoutClass {
    HostOversubscribed,
    CpuActive,
    Stalled,
    Unknown,
}

impl std::fmt::Display for TimeoutEvidence {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let class = match self.class {
            TimeoutClass::HostOversubscribed => "host-oversubscribed",
            TimeoutClass::CpuActive => "cpu-active",
            TimeoutClass::Stalled => "stalled",
            TimeoutClass::Unknown => "unknown",
        };
        write!(
            output,
            "timeout class={class} wall_ms={} tree_ticks={} host_busy_pct={} runnable={} cpus={}",
            self.elapsed.as_millis(),
            optional(self.tree_ticks),
            optional(self.host_busy_percent),
            optional(self.runnable),
            self.cpus,
        )
    }
}

fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

#[derive(Clone, Copy, Default)]
struct HostCpu {
    busy: u64,
    total: u64,
}

#[derive(Clone, Copy)]
struct EvidenceSample {
    tree_ticks: Option<u64>,
    host: Option<HostCpu>,
    runnable: Option<usize>,
    cpus: usize,
}

impl EvidenceSample {
    fn capture(process: u32) -> Self {
        Self {
            tree_ticks: tree_ticks(process),
            host: host_cpu(),
            runnable: runnable_processes(),
            cpus: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        }
    }
}

struct Progress {
    bytes: u64,
    ticks: Option<u64>,
    changed: Instant,
    sampled: Instant,
}

impl Progress {
    fn new(now: Instant, output: &Path, error: &Path, process: u32) -> Self {
        Self {
            bytes: capture_bytes(output, error),
            ticks: tree_ticks(process),
            changed: now,
            sampled: now,
        }
    }

    fn observe(&mut self, now: Instant, bytes: u64, ticks: Option<u64>, budget: Duration) -> bool {
        if now.duration_since(self.sampled) < SAMPLE_INTERVAL {
            return false;
        }
        self.sampled = now;
        if ticks.is_none() || bytes != self.bytes || ticks != self.ticks {
            self.bytes = bytes;
            self.ticks = ticks;
            self.changed = now;
            return false;
        }
        now.duration_since(self.changed) >= budget
    }
}

pub(super) fn configure(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }
}

pub(super) fn stall_budget(wall: Duration) -> Option<Duration> {
    let configured = std::env::var("HL_COMPAT_STALL_MS")
        .ok()
        .map_or(STALL_FLOOR, |value| {
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value != 0)
                .map(Duration::from_millis)
                .expect("HL_COMPAT_STALL_MS must be a positive integer")
        });
    (configured < wall).then_some(configured)
}

pub(super) fn wait(
    child: &mut Child,
    wall: Duration,
    stall: Option<Duration>,
    output: &Path,
    error: &Path,
) -> std::io::Result<Outcome> {
    let process = child.id();
    let started = Instant::now();
    let evidence = EvidenceSample::capture(process);
    let mut progress = Progress::new(started, output, error, process);
    let mut descendants = Descendants::default();
    loop {
        descendants.observe(process);
        let captured = capture_sizes(output, error);
        if exceeds_limit(captured) {
            terminate(child, &mut descendants)?;
            return Ok(Outcome::OutputLimit);
        }
        if let Some(status) = child.try_wait()? {
            return finish(process, &descendants, status, output, error);
        }
        if let Some(signal) = hl_engine::native::TerminationSignals::pending() {
            terminate(child, &mut descendants)?;
            return Ok(Outcome::Interrupted(signal));
        }
        let now = Instant::now();
        if now.duration_since(started) >= wall {
            let timeout = timeout_evidence(evidence, EvidenceSample::capture(process), now.duration_since(started));
            terminate(child, &mut descendants)?;
            return Ok(Outcome::TimedOut(timeout));
        }
        if stall.is_some_and(|budget| progress.observe(now, capture_bytes(output, error), tree_ticks(process), budget))
        {
            terminate(child, &mut descendants)?;
            return Ok(Outcome::Stalled);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn timeout_evidence(start: EvidenceSample, end: EvidenceSample, elapsed: Duration) -> TimeoutEvidence {
    let tree_ticks = match (start.tree_ticks, end.tree_ticks) {
        (Some(start), Some(end)) => Some(end.saturating_sub(start)),
        _ => None,
    };
    let host_busy_percent = match (start.host, end.host) {
        (Some(start), Some(end)) => {
            let total = end.total.saturating_sub(start.total);
            let busy = end.busy.saturating_sub(start.busy);
            (total != 0).then_some(((busy.saturating_mul(100) / total).min(100)) as u8)
        }
        _ => None,
    };
    let runnable = end.runnable;
    let oversubscribed = host_busy_percent.is_some_and(|busy| busy >= 90)
        && runnable.is_some_and(|runnable| runnable > end.cpus)
        && tree_ticks.is_some_and(|ticks| ticks != 0);
    let class = if oversubscribed {
        TimeoutClass::HostOversubscribed
    } else if tree_ticks.is_some_and(|ticks| ticks != 0) {
        TimeoutClass::CpuActive
    } else if tree_ticks == Some(0) {
        TimeoutClass::Stalled
    } else {
        TimeoutClass::Unknown
    };
    TimeoutEvidence {
        elapsed,
        tree_ticks,
        host_busy_percent,
        runnable,
        cpus: end.cpus,
        class,
    }
}

#[cfg(target_os = "linux")]
fn host_cpu() -> Option<HostCpu> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let fields = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let total = fields.iter().copied().fold(0_u64, u64::saturating_add);
    let idle = fields
        .get(3)
        .copied()
        .unwrap_or(0)
        .saturating_add(fields.get(4).copied().unwrap_or(0));
    Some(HostCpu {
        busy: total.saturating_sub(idle),
        total,
    })
}

#[cfg(target_os = "linux")]
fn runnable_processes() -> Option<usize> {
    let text = fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace().nth(3)?.split_once('/')?.0.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn host_cpu() -> Option<HostCpu> {
    None
}

#[cfg(not(target_os = "linux"))]
fn runnable_processes() -> Option<usize> {
    None
}

fn finish(
    process: u32,
    descendants: &Descendants,
    status: ExitStatus,
    output: &Path,
    error: &Path,
) -> std::io::Result<Outcome> {
    quiesce(process, descendants)?;
    Ok(if exceeds_limit(capture_sizes(output, error)) {
        Outcome::OutputLimit
    } else {
        Outcome::Exited(status)
    })
}

fn capture_sizes(output: &Path, error: &Path) -> (u64, u64) {
    let size = |path| fs::metadata(path).map_or(0, |metadata| metadata.len());
    (size(output), size(error))
}

fn capture_bytes(output: &Path, error: &Path) -> u64 {
    let (output, error) = capture_sizes(output, error);
    output.saturating_add(error)
}

fn exceeds_limit((output, error): (u64, u64)) -> bool {
    output > OUTPUT_LIMIT || error > ERROR_LIMIT
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct ProcessRecord {
    id: u32,
    parent: u32,
    ticks: u64,
    start: u64,
    descendant: bool,
}

#[cfg(target_os = "linux")]
fn read_process_table() -> Vec<ProcessRecord> {
    let mut processes = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return processes;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_str().and_then(|value| value.parse::<u32>().ok());
        let Some(id) = id else { continue };
        if processes.len() == PROCESS_LIMIT {
            return Vec::new();
        }
        if let Some(process) = process_record(id) {
            processes.push(process);
        }
    }
    processes
}

#[cfg(target_os = "linux")]
fn process_table() -> Vec<ProcessRecord> {
    struct Snapshot {
        sampled: Instant,
        processes: Vec<ProcessRecord>,
    }
    static SNAPSHOT: OnceLock<Mutex<Snapshot>> = OnceLock::new();
    let now = Instant::now();
    let snapshot = SNAPSHOT.get_or_init(|| {
        Mutex::new(Snapshot {
            sampled: now.checked_sub(PROCESS_SAMPLE_INTERVAL).unwrap_or(now),
            processes: Vec::new(),
        })
    });
    let mut snapshot = snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if now.duration_since(snapshot.sampled) >= PROCESS_SAMPLE_INTERVAL {
        snapshot.processes = read_process_table();
        snapshot.sampled = now;
    }
    snapshot.processes.clone()
}

#[cfg(target_os = "linux")]
fn process_record(id: u32) -> Option<ProcessRecord> {
    let text = fs::read_to_string(format!("/proc/{id}/stat")).ok()?;
    let (_, tail) = text.rsplit_once(") ")?;
    let fields = tail.split_whitespace().collect::<Vec<_>>();
    let parent = fields.get(1)?.parse::<u32>().ok()?;
    let user = fields.get(11)?.parse::<u64>().ok()?;
    let system = fields.get(12)?.parse::<u64>().ok()?;
    let start = fields.get(19)?.parse::<u64>().ok()?;
    Some(ProcessRecord {
        id,
        parent,
        ticks: user.saturating_add(system),
        start,
        descendant: false,
    })
}

#[cfg(target_os = "linux")]
fn tree_ticks(root: u32) -> Option<u64> {
    let descendants = process_tree(root);
    if descendants.is_empty() {
        return Some(0);
    }
    let processes = process_table();
    Some(
        descendants
            .into_iter()
            .filter_map(|(id, start)| {
                processes
                    .iter()
                    .find(|process| process.id == id && process.start == start)
                    .map(|process| process.ticks)
            })
            .fold(0_u64, u64::saturating_add),
    )
}

#[cfg(target_os = "linux")]
fn mark_descendants(processes: &mut [ProcessRecord], root: u32) {
    for process in &mut *processes {
        process.descendant = process.id == root;
    }
    for _ in 0..32 {
        let parents = processes
            .iter()
            .filter(|process| process.descendant)
            .map(|process| process.id)
            .collect::<Vec<_>>();
        if !mark_children(processes, &parents) {
            break;
        }
    }
}

#[cfg(target_os = "linux")]
fn mark_children(processes: &mut [ProcessRecord], parents: &[u32]) -> bool {
    let mut changed = false;
    for process in processes {
        if !process.descendant && parents.contains(&process.parent) {
            process.descendant = true;
            changed = true;
        }
    }
    changed
}

#[cfg(not(target_os = "linux"))]
fn tree_ticks(root: u32) -> Option<u64> {
    let _ = root;
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn signal_group(process: u32, signal: hl_engine::native::ProcessSignal) -> std::io::Result<bool> {
    use hl_engine::native::{HostError, ProcessId, ProcessSyscalls};

    let process = ProcessId::new(process).map_err(host_error)?;
    #[cfg(target_os = "linux")]
    let result = ProcessSyscalls::signal_group(&hl_engine::native::LinuxHost, process, signal);
    #[cfg(target_os = "macos")]
    let result = ProcessSyscalls::signal_group(&hl_engine::native::DarwinHost, process, signal);
    match result {
        Ok(()) => Ok(true),
        Err(HostError::NotFound) => Ok(false),
        Err(error) => Err(host_error(error)),
    }
}

#[cfg(target_os = "linux")]
fn signal_process(process: u32, signal: hl_engine::native::ProcessSignal) -> std::io::Result<bool> {
    use hl_engine::native::{HostError, LinuxHost, ProcessId, ProcessSyscalls};

    let process = ProcessId::new(process).map_err(host_error)?;
    match ProcessSyscalls::signal(&LinuxHost, process, signal) {
        Ok(()) => Ok(true),
        Err(HostError::NotFound) => Ok(false),
        Err(error) => Err(host_error(error)),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn host_error(error: hl_engine::native::HostError) -> std::io::Error {
    std::io::Error::other(format!("process group operation failed: {error:?}"))
}

#[derive(Default)]
struct Descendants {
    #[cfg(target_os = "linux")]
    processes: BTreeMap<u32, u64>,
}

impl Descendants {
    #[cfg(target_os = "linux")]
    fn observe(&mut self, root: u32) {
        for (process, start) in process_tree(root) {
            if process != root {
                self.processes.entry(process).or_insert(start);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn observe(&mut self, root: u32) {
        let _ = root;
    }

    #[cfg(target_os = "linux")]
    fn signal(&self, signal: hl_engine::native::ProcessSignal) -> std::io::Result<()> {
        for (&process, &start) in &self.processes {
            if process_start(process) == Some(start) {
                let _ = signal_process(process, signal)?;
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn signal(&self, signal: hl_engine::native::ProcessSignal) -> std::io::Result<()> {
        let _ = signal;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn gone(&self) -> bool {
        self.processes
            .iter()
            .all(|(&process, &start)| process_start(process) != Some(start))
    }

    #[cfg(not(target_os = "linux"))]
    fn gone(&self) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
fn process_tree(root: u32) -> Vec<(u32, u64)> {
    let mut processes = process_table();
    if !processes.iter().any(|process| process.id == root) {
        return Vec::new();
    }
    mark_descendants(&mut processes, root);
    processes
        .into_iter()
        .filter(|process| process.descendant)
        .map(|process| (process.id, process.start))
        .collect()
}

#[cfg(target_os = "linux")]
fn process_start(process: u32) -> Option<u64> {
    process_record(process).map(|record| record.start)
}

pub(super) fn contain(child: &mut Child) -> std::io::Result<()> {
    let mut descendants = Descendants::default();
    descendants.observe(child.id());
    terminate(child, &mut descendants)
}

fn terminate(child: &mut Child, descendants: &mut Descendants) -> std::io::Result<()> {
    let process = child.id();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = signal_group(process, hl_engine::native::ProcessSignal::Terminate)?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    child.kill()?;
    descendants.signal(hl_engine::native::ProcessSignal::Terminate)?;

    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        descendants.observe(process);
        if child.try_wait()?.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = signal_group(process, hl_engine::native::ProcessSignal::Kill);
    descendants.signal(hl_engine::native::ProcessSignal::Kill)?;
    let _ = child.kill();
    child.wait()?;
    quiesce(process, descendants)
}

fn quiesce(process: u32, descendants: &Descendants) -> std::io::Result<()> {
    let deadline = Instant::now() + REAP_GRACE;
    loop {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let group = signal_group(process, hl_engine::native::ProcessSignal::Kill)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let group = false;
        descendants.signal(hl_engine::native::ProcessSignal::Kill)?;
        if !group && descendants.gone() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "compatibility process containment did not quiesce",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod test {
    use super::{
        ERROR_LIMIT, EvidenceSample, HostCpu, OUTPUT_LIMIT, Progress, SAMPLE_INTERVAL, TimeoutClass, exceeds_limit,
        timeout_evidence,
    };
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn quiet_progress_stalls() {
        let start = Instant::now();
        let mut progress = Progress {
            bytes: 0,
            ticks: Some(7),
            changed: start,
            sampled: start,
        };
        assert!(!progress.observe(start + SAMPLE_INTERVAL, 0, Some(7), Duration::from_secs(2),));
        assert!(progress.observe(start + Duration::from_secs(2), 0, Some(7), Duration::from_secs(2),));
    }

    #[test]
    fn missing_cpu_progresses() {
        let start = Instant::now();
        let mut progress = Progress::new(start, Path::new("missing-out"), Path::new("missing-err"), u32::MAX);
        assert!(!progress.observe(start + Duration::from_secs(120), 0, None, Duration::from_secs(1),));
    }

    #[test]
    fn capture_limits_split() {
        assert!(!exceeds_limit((OUTPUT_LIMIT, ERROR_LIMIT)));
        assert!(exceeds_limit((OUTPUT_LIMIT + 1, 0)));
        assert!(exceeds_limit((0, ERROR_LIMIT + 1)));
    }

    #[test]
    fn timeout_classifies_host_starvation() {
        let start = EvidenceSample {
            tree_ticks: Some(10),
            host: Some(HostCpu { busy: 100, total: 200 }),
            runnable: Some(1),
            cpus: 4,
        };
        let end = EvidenceSample {
            tree_ticks: Some(12),
            host: Some(HostCpu { busy: 190, total: 300 }),
            runnable: Some(9),
            cpus: 4,
        };
        let evidence = timeout_evidence(start, end, Duration::from_secs(10));
        assert_eq!(evidence.class, TimeoutClass::HostOversubscribed);
        assert_eq!(evidence.tree_ticks, Some(2));
        assert_eq!(evidence.host_busy_percent, Some(90));
    }

    #[test]
    fn timeout_classifies_sleeping_child_as_stalled() {
        let start = EvidenceSample {
            tree_ticks: Some(7),
            host: Some(HostCpu { busy: 10, total: 20 }),
            runnable: Some(1),
            cpus: 4,
        };
        let end = EvidenceSample {
            tree_ticks: Some(7),
            host: Some(HostCpu { busy: 15, total: 30 }),
            runnable: Some(1),
            cpus: 4,
        };
        let evidence = timeout_evidence(start, end, Duration::from_secs(10));
        assert_eq!(evidence.class, TimeoutClass::Stalled);
        assert!(evidence.to_string().contains("class=stalled"));
    }

    #[test]
    fn timeout_does_not_blame_load_without_guest_progress() {
        let start = EvidenceSample {
            tree_ticks: Some(4),
            host: Some(HostCpu { busy: 0, total: 0 }),
            runnable: Some(1),
            cpus: 2,
        };
        let end = EvidenceSample {
            tree_ticks: Some(4),
            host: Some(HostCpu { busy: 99, total: 100 }),
            runnable: Some(20),
            cpus: 2,
        };
        assert_eq!(
            timeout_evidence(start, end, Duration::from_secs(10)).class,
            TimeoutClass::Stalled,
        );
    }
}
