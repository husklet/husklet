use clap::{Args, Subcommand, ValueEnum};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

mod adapter;
pub(crate) mod report;
mod workload;

const LIMIT: usize = 128;

/// The one phase whose `ok=` is an assertion rather than a work count. Every other
/// checksum counts iterations, so it is identical whatever the guest clock reports and
/// only `us=` moves; this phase reports 1 exactly when the guest's architectural timer
/// and `CLOCK_MONOTONIC` both honoured a 100ms sleep and agreed on its length.
pub(crate) const TIMEBASE_PHASE: &str = "timebase";
const TIMEBASE_FLOOR_US: u64 = 50_000;
const TIMEBASE_CEILING_US: u64 = 5_000_000;

/// Refuses a `timebase` row whose verdict is not 1, or whose own reported duration puts
/// the guest clock outside 2x of the sleep it just performed. The second bound is the
/// only check that survives a guest whose clocks are all wrong together, because it is
/// measured against the sleep the kernel actually served.
pub(crate) fn timebase_verdict(name: &str, us: u64, checksum: u64) -> Result<(), String> {
    if name != TIMEBASE_PHASE {
        return Ok(());
    }
    if checksum != 1 {
        return Err(format!(
            "phase {name} reported a divergent guest timebase (ok={checksum}); every us= on this arm is unsound"
        ));
    }
    if !(TIMEBASE_FLOOR_US..=TIMEBASE_CEILING_US).contains(&us) {
        return Err(format!(
            "phase {name} timed its own 100ms sleep as {us}us, outside {TIMEBASE_FLOOR_US}..={TIMEBASE_CEILING_US}"
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Isa {
    #[value(name = "arm64", alias = "aarch64")]
    Aarch64,
    #[value(name = "amd64", alias = "x86_64")]
    X86,
}

impl Isa {
    const fn name(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86 => "x86_64",
        }
    }

    const fn public(self) -> &'static str {
        match self {
            Self::Aarch64 => "arm64",
            Self::X86 => "amd64",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Provider {
    Native,
    Qemu,
    #[value(name = "c-engine")]
    C,
}

impl Provider {
    const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Qemu => "qemu",
            Self::C => "c-engine",
        }
    }
}

#[derive(Args, Clone, Debug)]
pub(crate) struct Run {
    #[arg(long, value_enum)]
    provider: Provider,
    #[arg(long = "arch", value_enum)]
    isa: Isa,
    #[arg(long)]
    binary: PathBuf,
    /// Materialized Linux root used for this provider invocation.
    #[arg(long)]
    rootfs: Option<PathBuf>,
    #[arg(long)]
    engine: Option<PathBuf>,
    /// Explicit launcher for retained-C engines whose packaging requires
    /// `RUNNER ENGINE GUEST`. Never pass a runner as `--engine`.
    #[arg(long)]
    c_runner: Option<PathBuf>,
    #[arg(long = "out")]
    output: Option<PathBuf>,
    #[arg(long, default_value_t = 5)]
    repeats: usize,
    #[arg(long, default_value = "120", value_parser = parse_duration)]
    timeout: Duration,
    #[arg(last = true, num_args = 0.., allow_hyphen_values = true)]
    guest: Vec<String>,
    #[arg(long = "env", value_parser = parse_assignment)]
    environment: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct Phase {
    time: u64,
    checksum: u64,
}

#[derive(Debug)]
struct Sample {
    phases: BTreeMap<String, Phase>,
    wall: u64,
    diagnostics: Vec<String>,
    x86_diagnostics: Option<adapter::X86Diagnostics>,
    causal_diagnostics: Option<adapter::CausalDiagnostics>,
}

#[derive(Default)]
struct Summary {
    checksum: Option<u64>,
    times: Vec<u64>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Execute a benchmark provider directly.
    Run(Run),
    /// Report the named workloads and the surfaces they do and do not cover.
    Workloads,
    /// Report provider reachability.
    List(List),
    /// Compare benchmark CSV reports.
    Report(report::Report),
}

#[derive(Args)]
pub(crate) struct List {
    #[arg(long = "arch", value_enum)]
    isa: Isa,
    #[arg(long)]
    binary: PathBuf,
    #[arg(long)]
    c_engine: Option<PathBuf>,
}

#[hl_design::adapter]
fn parse_duration(value: &str) -> Result<Duration, String> {
    let seconds = value.parse::<u64>().map_err(|_| "invalid timeout".to_owned())?;
    if seconds == 0 {
        Err("timeout must be positive".into())
    } else {
        Ok(Duration::from_secs(seconds))
    }
}

#[hl_design::adapter]
fn parse_assignment(value: &str) -> Result<(String, String), String> {
    let (name, value) = value.split_once('=').ok_or_else(|| "expected KEY=VALUE".to_owned())?;
    if name.is_empty() {
        Err("name cannot be empty".into())
    } else {
        Ok((name.into(), value.into()))
    }
}

fn reject_engine_only_environment(environment: &[(String, String)]) -> Result<(), String> {
    if let Some((name, _)) = environment
        .iter()
        .find(|(name, _)| name.starts_with("HL_NATIVE_") || name.starts_with("HL_A64_"))
    {
        return Err(format!("{name} is honoured only as --engine-option, not --env"));
    }
    Ok(())
}

/// Consolidated direct-benchmark command surface and its host process adapter.
pub struct Application {
    process: adapter::Process,
}

impl Application {
    pub fn new(search_path: Option<OsString>) -> Self {
        Self {
            process: adapter::Process::new(search_path),
        }
    }

    pub fn execute(&self, command: Command) -> Result<(), String> {
        match command {
            Command::Run(run) => run.validate()?.execute(&self.process),
            Command::Workloads => {
                workload::report();
                Ok(())
            }
            Command::Report(report) => report.write(),
            Command::List(options) => self.list(options),
        }
    }

    fn list(&self, options: List) -> Result<(), String> {
        let List { isa, binary, c_engine } = options;
        let host = std::env::consts::ARCH;
        let native = matches!((host, isa), ("aarch64", Isa::Aarch64) | ("x86_64", Isa::X86));
        let rows = [
            (
                "native",
                adapter::Process::executable(&binary) && native,
                format!("host={host}"),
            ),
            (
                "qemu",
                adapter::Process::executable(&binary) && self.process.available(&format!("qemu-{}", isa.name())),
                format!("qemu-{}", isa.name()),
            ),
            (
                "c-engine",
                adapter::Process::executable(&binary) && c_engine.as_deref().is_some_and(adapter::Process::executable),
                c_engine.map_or_else(|| "not configured".into(), |path| path.display().to_string()),
            ),
        ];
        println!("provider,arch,reachable,detail");
        for (provider, reachable, detail) in rows {
            println!("{provider},{},{reachable},{detail}", isa.public());
        }
        Ok(())
    }
}

impl Run {
    fn validate(self) -> Result<Self, String> {
        if self.repeats == 0 || self.repeats > LIMIT {
            return Err(format!("repeats must be between 1 and {LIMIT}"));
        }
        if self.timeout.is_zero() {
            return Err("timeout must be positive".into());
        }
        if self.provider == Provider::C && self.engine.is_none() {
            return Err("engine provider requires --engine".into());
        }
        if self.c_runner.is_some() && self.provider != Provider::C {
            return Err("--c-runner is valid only with provider c-engine".into());
        }
        if let Some(rootfs) = &self.rootfs {
            if !rootfs.is_dir() {
                return Err(format!("rootfs does not exist: {}", rootfs.display()));
            }
            if !self.binary.is_absolute()
                || self
                    .binary
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err("rootfs guest executable must be an absolute confined path".into());
            }
        }
        // The engine reads these only from --engine-option, so accepting them as
        // guest environment would apply nothing while looking like it did.
        reject_engine_only_environment(&self.environment)?;
        Ok(self)
    }

    fn diagnostics_mode(&self) -> &'static str {
        "off"
    }

    fn execution_mode(&self) -> &'static str {
        match self.provider {
            Provider::Native => "host-native",
            Provider::Qemu => "qemu",
            Provider::C => "c-engine",
        }
    }

    fn execute(self, process: &adapter::Process) -> Result<(), String> {
        let affinity = host_affinity();
        let guest_identity = file_identity(&self.host_binary())?;
        let engine_identity = self
            .engine
            .as_deref()
            .map(file_identity)
            .transpose()?
            .unwrap_or_else(|| "native".into());
        let runner_identity = crate::runtime::profile::runner()
            .map_err(|error| error.to_string())
            .and_then(|path| file_identity(&path))?;
        let options_identity = self.options_identity();
        let mut phases: BTreeMap<String, Summary> = BTreeMap::new();
        let mut walls = Vec::with_capacity(self.repeats);
        for repetition in 0..self.repeats {
            let sample = process.sample(&self)?;
            walls.push(sample.wall);
            if let Some(diagnostics) = sample.x86_diagnostics {
                report_x86_diagnostics(repetition + 1, &diagnostics);
            }
            if let Some(diagnostics) = sample.causal_diagnostics {
                eprintln!(
                    "diagnostic repeat={} relocation_cold_targets={} relocation_cycles={} relocation_capacity={} relocation_invalidations={} ibtc_site_misses={} ibtc_shared_misses={}",
                    repetition + 1,
                    diagnostics.relocation_cold_targets,
                    diagnostics.relocation_cycles,
                    diagnostics.relocation_capacity,
                    diagnostics.relocation_invalidations,
                    diagnostics.ibtc_site_misses,
                    diagnostics.ibtc_shared_misses,
                );
            }
            for line in sample.diagnostics {
                eprintln!("diagnostic repeat={} {line}", repetition + 1);
            }
            for (name, phase) in sample.phases {
                let summary = phases.entry(name.clone()).or_default();
                summary.observe(&name, phase)?;
            }
        }
        if phases.values().any(|phase| phase.times.len() != self.repeats) {
            return Err("phase set changed across repeats".into());
        }
        let mut writer: Box<dyn Write> = match &self.output {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| format!("output directory: {error}"))?;
                }
                Box::new(File::create(path).map_err(|error| format!("output file: {error}"))?)
            }
            None => Box::new(io::stdout()),
        };
        writeln!(writer, "env,arch,phase,us,ok,us_min,us_max,repeats,wall_us,execution,diagnostics,phase_context,guest_sha256,engine_sha256,runner_sha256,options_sha256,cpu_affinity")
            .map_err(|error| error.to_string())?;
        let wall = Summary::median(&mut walls);
        // A phase run alone in its process is cold; one run mid-sequence inherits
        // translations and suppression state, so the two are not comparable.
        let context = match phases.len() {
            1 => "isolated".to_owned(),
            count => format!("sequence-of-{count}"),
        };
        for (name, mut phase) in phases {
            let minimum = *phase.times.iter().min().expect("nonempty phase");
            let maximum = *phase.times.iter().max().expect("nonempty phase");
            let time = Summary::median(&mut phase.times);
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                self.provider.name(),
                self.isa.public(),
                name,
                time,
                phase.checksum.expect("validated checksum"),
                minimum,
                maximum,
                self.repeats,
                wall,
                self.execution_mode(),
                self.diagnostics_mode(),
                context,
                guest_identity,
                engine_identity,
                runner_identity,
                options_identity,
                affinity.replace(',', ";"),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn options_identity(&self) -> String {
        let mut digest = sha2::Sha256::default();
        for value in [
            self.provider.name(),
            self.isa.public(),
            self.execution_mode(),
            self.diagnostics_mode(),
            &self.repeats.to_string(),
            &self.timeout.as_nanos().to_string(),
        ] {
            identity_field(&mut digest, value.as_bytes());
        }
        for (name, value) in &self.environment {
            identity_field(&mut digest, name.as_bytes());
            identity_field(&mut digest, value.as_bytes());
        }
        for argument in &self.guest {
            identity_field(&mut digest, argument.as_bytes());
        }
        if let Some(rootfs) = &self.rootfs {
            identity_field(&mut digest, rootfs.as_os_str().as_encoded_bytes());
        }
        identity_field(&mut digest, host_affinity().as_bytes());
        crate::record::FramedIdentity::hex(&digest.finalize())
    }

    fn host_binary(&self) -> PathBuf {
        self.rootfs.as_ref().map_or_else(
            || self.binary.clone(),
            |rootfs| {
                rootfs.join(
                    self.binary
                        .strip_prefix("/")
                        .expect("validated absolute guest executable"),
                )
            },
        )
    }
}

mod diagnostic;
use diagnostic::report_x86_diagnostics;
fn identity_field(digest: &mut sha2::Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}

/// Reports the identity of a file, spelling failures the way the benchmark reports them.
pub(super) fn file_identity(path: &std::path::Path) -> Result<String, String> {
    crate::record::FramedIdentity::of_file(path).map_err(|error| format!("hash {}: {error}", path.display()))
}

#[cfg(target_os = "linux")]
fn host_affinity() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Cpus_allowed_list:\t"))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(not(target_os = "linux"))]
fn host_affinity() -> String {
    "unspecified".into()
}

impl Phase {
    fn parse(line: &str) -> Result<Option<(String, Self)>, String> {
        let Some(rest) = line.strip_prefix("PHASE ") else {
            return Ok(None);
        };
        let mut fields = rest.split_whitespace();
        let name = fields.next().ok_or_else(|| format!("invalid phase row: {line}"))?;
        let time = fields
            .next()
            .and_then(|value| value.strip_prefix("us="))
            .ok_or_else(|| format!("invalid phase time: {line}"))?
            .parse()
            .map_err(|_| format!("invalid phase time: {line}"))?;
        let checksum = fields
            .next()
            .and_then(|value| value.strip_prefix("ok="))
            .ok_or_else(|| format!("invalid phase checksum: {line}"))?
            .parse()
            .map_err(|_| format!("invalid phase checksum: {line}"))?;
        // Every phase counts the work it completed, so zero is a silent no-op, not a pass.
        if checksum == 0 {
            return Err(format!("phase {name} completed no work (ok=0)"));
        }
        timebase_verdict(name, time, checksum)?;
        Ok(Some((name.into(), Self { time, checksum })))
    }
}

impl Summary {
    fn observe(&mut self, name: &str, phase: Phase) -> Result<(), String> {
        if self.checksum.is_some_and(|expected| expected != phase.checksum) {
            return Err(format!("checksum changed across repeats for {name}"));
        }
        self.checksum = Some(phase.checksum);
        self.times.push(phase.time);
        Ok(())
    }

    fn median(values: &mut [u64]) -> u64 {
        values.sort_unstable();
        if values.len().is_multiple_of(2) {
            values[values.len() / 2 - 1].saturating_add(values[values.len() / 2]) / 2
        } else {
            values[values.len() / 2]
        }
    }
}

#[cfg(test)]
mod test;
