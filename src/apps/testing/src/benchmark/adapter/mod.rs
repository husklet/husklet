use super::{Phase, Run, Sample};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod command;

const CAPTURE_LIMIT: usize = 1024 * 1024;

/// Application-owned host process adapter for benchmark discovery and execution.
pub(super) struct Process {
    search_path: Option<OsString>,
}

impl Process {
    pub(super) fn new(search_path: Option<OsString>) -> Self {
        Self { search_path }
    }

    pub(super) fn sample(&self, run: &Run) -> Result<Sample, String> {
        let mut command = self.command(run)?;
        let start = Instant::now();
        let mut child = command.spawn().map_err(|error| format!("spawn failed: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "missing stdout capture".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "missing stderr capture".to_string())?;
        let stdout = std::thread::spawn(move || Self::capture_output(stdout));
        let stderr = std::thread::spawn(move || Self::capture_output(stderr));
        let mut timed_out = false;
        let mut tree = Vec::new();
        loop {
            for process in Self::descendants(child.id()) {
                if !tree.contains(&process) {
                    tree.push(process);
                }
            }
            match child.try_wait().map_err(|error| format!("wait failed: {error}"))? {
                Some(_) => break,
                None if start.elapsed() < run.timeout => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    timed_out = true;
                    Self::terminate(child.id(), &tree);
                    let _ = child.kill();
                    break;
                }
            }
        }
        let wall = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
        let status = child.wait().map_err(|error| format!("reap failed: {error}"))?;
        let stdout = stdout.join().map_err(|_| "stdout capture panicked".to_string())??;
        let stderr = stderr.join().map_err(|_| "stderr capture panicked".to_string())??;
        if timed_out {
            return Err(format!("timed out after {}s", run.timeout.as_secs()));
        }
        if !status.success() {
            return Err(format!(
                "guest failed with {status}: {}",
                String::from_utf8_lossy(&stderr).trim()
            ));
        }
        let diagnostics_text = String::from_utf8_lossy(&stderr);
        if run.native_requested() {
            match Self::native_runs(&diagnostics_text) {
                Some(0) => {
                    return Err("native execution was requested but diagnostics report zero native runs".into());
                }
                Some(_) => {}
                None => return Err("native execution was requested but native diagnostics are missing".into()),
            }
        }
        let text = String::from_utf8(stdout).map_err(|_| "guest output is not UTF-8".to_string())?;
        let mut phases = BTreeMap::new();
        for line in text.lines() {
            if let Some((name, phase)) = Phase::parse(line)? {
                phases.insert(name, phase);
            }
        }
        if phases.is_empty() {
            return Err("guest emitted no PHASE rows".into());
        }
        let diagnostics = diagnostics_text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        Ok(Sample {
            phases,
            wall,
            diagnostics,
        })
    }

    fn native_runs(diagnostics: &str) -> Option<u64> {
        diagnostics
            .lines()
            .filter_map(|line| line.strip_prefix("hl-native-detail: "))
            .flat_map(str::split_whitespace)
            .filter_map(|field| field.split_once('='))
            .filter(|(name, _)| matches!(*name, "branch" | "syscall" | "fallback" | "yield" | "completed"))
            .filter_map(|(_, value)| value.parse::<u64>().ok())
            .reduce(u64::saturating_add)
    }

    fn capture_output(mut reader: impl Read) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        reader
            .by_ref()
            .take(u64::try_from(CAPTURE_LIMIT).expect("capture limit fits u64") + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("capture failed: {error}"))?;
        if bytes.len() > CAPTURE_LIMIT {
            return Err(format!("output exceeded {CAPTURE_LIMIT} bytes"));
        }
        Ok(bytes)
    }

    #[cfg(target_os = "linux")]
    fn descendants(root: u32) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut parents = Vec::new();
        for entry in entries.flatten() {
            let Some(process) = entry.file_name().to_str().and_then(|value| value.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some((_, fields)) = stat.rsplit_once(") ") else {
                continue;
            };
            if let Some(parent) = fields
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<u32>().ok())
            {
                parents.push((process, parent));
            }
        }
        let mut tree = vec![root];
        let mut index = 0;
        while index < tree.len() {
            let parent = tree[index];
            for &(process, owner) in &parents {
                if owner == parent && !tree.contains(&process) {
                    tree.push(process);
                }
            }
            index += 1;
        }
        tree.into_iter().skip(1).collect()
    }

    #[cfg(not(target_os = "linux"))]
    fn descendants(_root: u32) -> Vec<u32> {
        Vec::new()
    }

    #[cfg(unix)]
    fn signal(signal: &str, target: &str) {
        let _ = Command::new("kill")
            .args([signal, "--", target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(unix)]
    fn terminate(process: u32, descendants: &[u32]) {
        let group = format!("-{process}");
        Self::signal("-TERM", &group);
        for process in descendants {
            Self::signal("-TERM", &process.to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
        Self::signal("-KILL", &group);
        for process in descendants {
            Self::signal("-KILL", &process.to_string());
        }
    }

    #[cfg(not(unix))]
    fn terminate(_process: u32, _descendants: &[u32]) {}
}

#[cfg(test)]
mod test {
    use super::Process;

    #[test]
    fn native_diagnostics() {
        assert_eq!(
            Process::native_runs(
                "hl-native-detail: fills=1 site_collisions=0 shared_collisions=0 branch=7 syscall=2 fallback=0 yield=3 completed=1 operand_callbacks=0 operand_cache_hits=0"
            ),
            Some(13),
        );
        assert_eq!(
            Process::native_runs(
                "hl-native-detail: fills=4 site_collisions=0 shared_collisions=0 branch=0 syscall=0 fallback=0 yield=0 completed=0 operand_callbacks=0 operand_cache_hits=0"
            ),
            Some(0),
        );
        assert_eq!(Process::native_runs("unrelated diagnostic"), None);
    }
}
