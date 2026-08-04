use super::{Phase, Run, Sample};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod command;

const CAPTURE_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86Diagnostics {
    pub(super) public_exits: u64,
    pub(super) public_syscalls: u64,
    pub(super) syscall_vector_dirty: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct CausalDiagnostics {
    pub(super) relocation_cold_targets: u64,
    pub(super) relocation_cycles: u64,
    pub(super) relocation_capacity: u64,
    pub(super) relocation_invalidations: u64,
    pub(super) ibtc_site_misses: u64,
    pub(super) ibtc_shared_misses: u64,
}

impl X86Diagnostics {
    pub(super) fn dirty_share_ppm(self) -> Option<u64> {
        (self.public_syscalls != 0)
            .then(|| {
                u64::try_from(u128::from(self.syscall_vector_dirty) * 1_000_000 / u128::from(self.public_syscalls)).ok()
            })
            .flatten()
    }
}

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
        let x86_diagnostics = Self::x86_diagnostics(&diagnostics_text)?;
        let causal_diagnostics = Self::causal_diagnostics(&diagnostics_text)?;
        Ok(Sample {
            phases,
            wall,
            diagnostics,
            x86_diagnostics,
            causal_diagnostics,
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

    fn x86_diagnostics(diagnostics: &str) -> Result<Option<X86Diagnostics>, String> {
        let mut total = None::<X86Diagnostics>;
        for line in diagnostics
            .lines()
            .filter_map(|line| line.strip_prefix("hl-native-detail: "))
        {
            let mut exits = None;
            let mut syscalls = None;
            let mut dirty = None;
            for (name, value) in line.split_whitespace().filter_map(|field| field.split_once('=')) {
                let destination = match name {
                    "x86_public_exits" => &mut exits,
                    "x86_public_syscalls" => &mut syscalls,
                    "x86_syscall_vector_dirty" => &mut dirty,
                    _ => continue,
                };
                *destination = value.parse::<u64>().ok();
            }
            let observed =
                usize::from(exits.is_some()) + usize::from(syscalls.is_some()) + usize::from(dirty.is_some());
            if observed == 0 {
                continue;
            }
            let (Some(public_exits), Some(public_syscalls), Some(syscall_vector_dirty)) = (exits, syscalls, dirty)
            else {
                return Ok(None);
            };
            let current = total.get_or_insert(X86Diagnostics {
                public_exits: 0,
                public_syscalls: 0,
                syscall_vector_dirty: 0,
            });
            current.public_exits = current
                .public_exits
                .checked_add(public_exits)
                .ok_or_else(|| "native x86 public exit diagnostics overflow".to_owned())?;
            current.public_syscalls = current
                .public_syscalls
                .checked_add(public_syscalls)
                .ok_or_else(|| "native x86 public syscall diagnostics overflow".to_owned())?;
            current.syscall_vector_dirty = current
                .syscall_vector_dirty
                .checked_add(syscall_vector_dirty)
                .ok_or_else(|| "native x86 vector-dirty diagnostics overflow".to_owned())?;
        }
        Ok(total)
    }

    fn causal_diagnostics(diagnostics: &str) -> Result<Option<CausalDiagnostics>, String> {
        const NAMES: [&str; 6] = [
            "relocation_cold_targets",
            "relocation_cycles",
            "relocation_capacity",
            "relocation_invalidations",
            "ibtc_site_misses",
            "ibtc_shared_misses",
        ];
        let mut total = None::<CausalDiagnostics>;
        for line in diagnostics
            .lines()
            .filter_map(|line| line.strip_prefix("hl-native-detail: "))
        {
            let mut values = [None; 6];
            for (name, value) in line.split_whitespace().filter_map(|field| field.split_once('=')) {
                let Some(index) = NAMES.iter().position(|candidate| *candidate == name) else {
                    continue;
                };
                values[index] = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid native causal diagnostic {name}"))?,
                );
            }
            let observed = values.iter().filter(|value| value.is_some()).count();
            if observed == 0 {
                continue;
            }
            if observed != values.len() {
                return Ok(None);
            }
            let current = CausalDiagnostics {
                relocation_cold_targets: values[0].unwrap_or_default(),
                relocation_cycles: values[1].unwrap_or_default(),
                relocation_capacity: values[2].unwrap_or_default(),
                relocation_invalidations: values[3].unwrap_or_default(),
                ibtc_site_misses: values[4].unwrap_or_default(),
                ibtc_shared_misses: values[5].unwrap_or_default(),
            };
            let accumulated = total.get_or_insert_with(CausalDiagnostics::default);
            accumulated.relocation_cold_targets = accumulated
                .relocation_cold_targets
                .checked_add(current.relocation_cold_targets)
                .ok_or_else(|| "native relocation cold-target diagnostics overflow".to_owned())?;
            accumulated.relocation_cycles = accumulated
                .relocation_cycles
                .checked_add(current.relocation_cycles)
                .ok_or_else(|| "native relocation cycle diagnostics overflow".to_owned())?;
            accumulated.relocation_capacity = accumulated
                .relocation_capacity
                .checked_add(current.relocation_capacity)
                .ok_or_else(|| "native relocation capacity diagnostics overflow".to_owned())?;
            accumulated.relocation_invalidations = accumulated
                .relocation_invalidations
                .checked_add(current.relocation_invalidations)
                .ok_or_else(|| "native relocation invalidation diagnostics overflow".to_owned())?;
            accumulated.ibtc_site_misses = accumulated
                .ibtc_site_misses
                .checked_add(current.ibtc_site_misses)
                .ok_or_else(|| "native IBTC site-miss diagnostics overflow".to_owned())?;
            accumulated.ibtc_shared_misses = accumulated
                .ibtc_shared_misses
                .checked_add(current.ibtc_shared_misses)
                .ok_or_else(|| "native IBTC shared-miss diagnostics overflow".to_owned())?;
        }
        Ok(total)
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
    use super::{CausalDiagnostics, Process, X86Diagnostics};

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

    #[test]
    fn x86_diagnostics_are_optional_ordered_fields() {
        assert_eq!(Process::x86_diagnostics("old detail without counters").unwrap(), None);
        assert_eq!(
            Process::x86_diagnostics(
                "hl-native-detail: unknown=9 x86_public_syscalls=8 x86_syscall_vector_dirty=3 x86_public_exits=10"
            )
            .unwrap(),
            Some(X86Diagnostics {
                public_exits: 10,
                public_syscalls: 8,
                syscall_vector_dirty: 3,
            }),
        );
    }

    #[test]
    fn x86_diagnostics_aggregate_complete_executors() {
        let diagnostics = Process::x86_diagnostics(
            "hl-native-detail: x86_public_exits=10 x86_public_syscalls=8 x86_syscall_vector_dirty=3\n\
             hl-native-detail: x86_syscall_vector_dirty=1 x86_public_exits=5 x86_public_syscalls=2",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            diagnostics,
            X86Diagnostics {
                public_exits: 15,
                public_syscalls: 10,
                syscall_vector_dirty: 4,
            }
        );
        assert_eq!(diagnostics.dirty_share_ppm(), Some(400_000));
    }

    #[test]
    fn partial_or_zero_x86_diagnostics_have_no_share() {
        assert_eq!(
            Process::x86_diagnostics("hl-native-detail: x86_public_exits=10 x86_public_syscalls=8").unwrap(),
            None,
        );
        assert_eq!(
            X86Diagnostics {
                public_exits: 0,
                public_syscalls: 0,
                syscall_vector_dirty: 0,
            }
            .dirty_share_ppm(),
            None,
        );
        assert_eq!(
            X86Diagnostics {
                public_exits: u64::MAX,
                public_syscalls: 1,
                syscall_vector_dirty: u64::MAX,
            }
            .dirty_share_ppm(),
            None,
        );
        assert!(
            Process::x86_diagnostics(
                "hl-native-detail: x86_public_exits=18446744073709551615 x86_public_syscalls=0 x86_syscall_vector_dirty=0\n\
                 hl-native-detail: x86_public_exits=1 x86_public_syscalls=0 x86_syscall_vector_dirty=0"
            )
            .is_err()
        );
    }

    #[test]
    fn causal_diagnostics_are_named_and_optional() {
        assert_eq!(Process::causal_diagnostics("hl-native-detail: branch=1").unwrap(), None);
        let line = "hl-native-detail: ibtc_shared_misses=6 relocation_cycles=2 relocation_cold_targets=1 relocation_capacity=3 ibtc_site_misses=5 relocation_invalidations=4";
        assert_eq!(
            Process::causal_diagnostics(line).unwrap(),
            Some(CausalDiagnostics {
                relocation_cold_targets: 1,
                relocation_cycles: 2,
                relocation_capacity: 3,
                relocation_invalidations: 4,
                ibtc_site_misses: 5,
                ibtc_shared_misses: 6,
            })
        );
        assert_eq!(
            Process::causal_diagnostics("hl-native-detail: relocation_cold_targets=1 relocation_cycles=2")
                .unwrap(),
            None
        );
        assert!(Process::causal_diagnostics(
            "hl-native-detail: relocation_cold_targets=no relocation_cycles=0 relocation_capacity=0 relocation_invalidations=0 ibtc_site_misses=0 ibtc_shared_misses=0"
        )
        .is_err());
    }
}
