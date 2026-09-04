//! Ephemeral engine-only benchmark evidence.

use crate::{Error, Result};
use nix::{sys::signal::{kill, Signal}, unistd::Pid};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkMeasurement {
    pub path: PathBuf,
    pub raw: String,
}

pub(crate) struct Collector {
    path: PathBuf,
    child: Child,
    control: ChildStdin,
    acknowledge: BufReader<ChildStdout>,
    enabled_at: Option<std::time::Instant>,
}

impl Collector {
    pub(crate) fn prepare(path: &Path) -> Result<Self> {
        if path.exists() {
            return Err(Error::Runtime("benchmark measurement output already exists".into()));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if path.exists() {
            return Err(Error::Runtime(format!("benchmark measurement already exists: {}", path.display())));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut child = Command::new("perf")
            .args(["stat", "--delay=-1", "--control", "fd:0,1", "--inherit", "-x", "\t", "-o"])
            .arg(path)
            .args(["-e", "duration_time,task-clock,instructions,cycles,page-faults", "-p"])
            .arg(std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Error::Runtime(format!("start benchmark collector: {error}")))?;
        let control = child.stdin.take().ok_or_else(|| Error::Runtime("collector control unavailable".into()))?;
        let acknowledge = BufReader::new(
            child.stdout.take().ok_or_else(|| Error::Runtime("collector acknowledgement unavailable".into()))?,
        );
        Ok(Self { path: path.to_owned(), child, control, acknowledge, enabled_at: None })
    }

    pub(crate) fn enable(&mut self) -> Result<()> {
        self.command("enable")?;
        self.enabled_at = Some(std::time::Instant::now());
        Ok(())
    }

    fn command(&mut self, command: &str) -> Result<()> {
        writeln!(self.control, "{command}")?;
        self.control.flush()?;
        let mut line = String::new();
        self.acknowledge.read_line(&mut line)?;
        if line.trim_matches(|character: char| character.is_whitespace() || character == '\0') != "ack" {
            return Err(Error::Runtime(format!("collector did not acknowledge {command}: {line:?}")));
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<BenchmarkMeasurement> {
        // Timestamp before disable: control teardown latency is not guest execution.
        let wall_ns = u64::try_from(
            self.enabled_at
                .take()
                .ok_or_else(|| Error::Runtime("benchmark collector was never enabled".into()))?
                .elapsed()
                .as_nanos(),
        )
        .map_err(|_| Error::Runtime("engine measurement wall duration overflowed".into()))?;
        self.command("disable")?;
        // perf has no control command that both snapshots and exits an attached task. SIGINT is
        // its documented graceful completion path and flushes the final stat record.
        kill(Pid::from_raw(i32::try_from(self.child.id()).unwrap_or(i32::MAX)), Signal::SIGINT)
            .map_err(|error| Error::Runtime(format!("stop benchmark collector: {error}")))?;
        let status = self.child.wait()?;
        if !status.success() && status.signal() != Some(2) {
            return Err(Error::Runtime(format!("benchmark collector failed: {status}")));
        }
        let mut raw = fs::read_to_string(&self.path)?;
        raw.push_str(&format!("# husklet-engine-wall-ns={wall_ns}\n"));
        fs::write(&self.path, &raw)?;
        Ok(BenchmarkMeasurement { path: self.path.clone(), raw })
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::Collector;

    #[cfg(target_os = "linux")]
    #[test]
    fn collector_acknowledges_enable_and_flushes_a_typed_result() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine.perf");
        let mut collector = Collector::prepare(&path).unwrap();
        collector.enable().unwrap();
        let mut value = 0u64;
        for number in 0..1_000_000u64 {
            value = value.wrapping_add(number);
        }
        std::hint::black_box(value);
        let result = collector.finish().unwrap();
        assert_eq!(result.path, path);
        assert!(result.raw.contains("instructions"));
        assert!(result.raw.contains("duration_time"));
        assert!(result.raw.contains("# husklet-engine-wall-ns="));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delayed_collector_setup_is_excluded_from_engine_wall() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delayed.perf");
        let total = std::time::Instant::now();
        let mut collector = Collector::prepare(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        collector.enable().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let result = collector.finish().unwrap();
        let wall_ns = result.raw.lines()
            .find_map(|line| line.strip_prefix("# husklet-engine-wall-ns="))
            .unwrap().parse::<u64>().unwrap();
        assert!(total.elapsed().as_nanos() > u128::from(wall_ns) + 50_000_000);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_a_prepared_collector_reaps_perf() {
        let directory = tempfile::tempdir().unwrap();
        let collector = Collector::prepare(&directory.path().join("drop.perf")).unwrap();
        let pid = nix::unistd::Pid::from_raw(i32::try_from(collector.child.id()).unwrap());
        drop(collector);
        assert_eq!(
            nix::sys::signal::kill(pid, None).unwrap_err(),
            nix::errno::Errno::ESRCH
        );
    }

    #[test]
    fn engine_enablement_stays_after_all_launch_preparation() {
        let source = include_str!("engine/mod.rs");
        let members = source.find("let members = config").unwrap();
        let prepare = source.find("Collector::prepare").unwrap();
        let enable = source.find("collector.enable()").unwrap();
        let start = source.find("engine\n            .start()").unwrap();
        assert!(members < prepare && prepare < enable && enable < start);
    }

    #[test]
    fn every_wait_setup_failure_finishes_the_collector() {
        let source = include_str!("engine/process.rs");
        let spawn_failure = source.find("engine wait thread: {error}").unwrap();
        let channel_failure = source.find("engine wait thread ended without a result").unwrap();
        for failure in [spawn_failure, channel_failure] {
            let branch = &source[failure.saturating_sub(180)..failure];
            assert!(branch.contains("finish_benchmark_collector()?"));
        }
    }

    #[test]
    fn removing_an_unstarted_container_discards_its_ephemeral_request() {
        let source = include_str!("service/container/removal.rs");
        let durable_remove = source.find("self.containers.remove(&container.id).await?").unwrap();
        let ephemeral_remove = source.find("self.measurement_requests.lock().await.remove(&container.id)").unwrap();
        assert!(durable_remove < ephemeral_remove);
    }
}
