use clap::{Args, Subcommand, ValueEnum};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

mod adapter;
mod alternating;
mod gate;
mod matrix;
pub(crate) mod report;
mod workload;

use matrix::Matrix;

const LIMIT: usize = 128;

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
    #[value(name = "rust-engine")]
    Rust,
}

impl Provider {
    const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Qemu => "qemu",
            Self::C => "c-engine",
            Self::Rust => "rust-engine",
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
    #[arg(long = "engine-option", value_parser = parse_assignment)]
    engine_options: Vec<(String, String)>,
    /// The owning matrix recorded a diagnostics-on proof before this row.
    #[arg(skip)]
    diagnostics_proven: bool,
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
    /// Run the same guest through native, retained-C, and Rust engines.
    Matrix(Matrix),
    /// Judge one named workload against the committed Rust baseline.
    Gate(gate::Gate),
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
    #[arg(long)]
    rust_engine: Option<PathBuf>,
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let seconds = value.parse::<u64>().map_err(|_| "invalid timeout".to_owned())?;
    if seconds == 0 {
        Err("timeout must be positive".into())
    } else {
        Ok(Duration::from_secs(seconds))
    }
}

fn parse_assignment(value: &str) -> Result<(String, String), String> {
    let (name, value) = value.split_once('=').ok_or_else(|| "expected KEY=VALUE".to_owned())?;
    if name.is_empty() {
        Err("name cannot be empty".into())
    } else {
        Ok((name.into(), value.into()))
    }
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
            Command::Matrix(matrix) => matrix.validate()?.execute(&self.process),
            Command::Gate(gate) => gate.execute(&self.process),
            Command::Workloads => {
                workload::report();
                Ok(())
            }
            Command::Report(report) => report.write(),
            Command::List(options) => self.list(options),
        }
    }

    fn list(&self, options: List) -> Result<(), String> {
        let List {
            isa,
            binary,
            c_engine,
            rust_engine,
        } = options;
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
            (
                "rust-engine",
                adapter::Process::executable(&binary)
                    && rust_engine.as_deref().is_some_and(adapter::Process::executable),
                rust_engine.map_or_else(|| "not configured".into(), |path| path.display().to_string()),
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
        if matches!(self.provider, Provider::C | Provider::Rust) && self.engine.is_none() {
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
        if self.provider != Provider::Rust && !self.engine_options.is_empty() {
            return Err("--engine-option is supported only by rust-engine".into());
        }
        // The engine reads these only from --engine-option, so accepting them as
        // guest environment would apply nothing while looking like it did.
        if let Some((name, _)) = self.environment.iter().find(|(name, _)| name.starts_with("HL_NATIVE_")) {
            return Err(format!("{name} is honoured only as --engine-option, not --env"));
        }
        Ok(self)
    }

    /// A native timing row must be proven native, but proving it turns on
    /// per-boundary capture, so the proof runs as its own throwaway sample.
    fn diagnostics_probe(&self) -> Option<Self> {
        if !self.native_requested() || self.diagnostics_proven || self.native_diagnostics_requested() {
            return None;
        }
        let mut probe = self.clone();
        probe.engine_options.push(("HL_NATIVE_DIAGNOSTICS".into(), "1".into()));
        probe.diagnostics_proven = true;
        probe.repeats = 1;
        probe.output = None;
        Some(probe)
    }

    fn diagnostics_mode(&self) -> &'static str {
        if self.native_diagnostics_requested() {
            "on"
        } else {
            "off"
        }
    }

    fn native_requested(&self) -> bool {
        self.provider == Provider::Rust
            && self
                .engine_options
                .iter()
                .any(|(name, value)| name == "HL_NATIVE_EXECUTION" && value == "1")
    }

    fn native_diagnostics_requested(&self) -> bool {
        self.native_requested()
            && self
                .engine_options
                .iter()
                .any(|(name, value)| name == "HL_NATIVE_DIAGNOSTICS" && value == "1")
    }

    fn execution_mode(&self) -> &'static str {
        match (self.provider, self.native_requested()) {
            (Provider::Native, _) => "host-native",
            (Provider::Qemu, _) => "qemu",
            (Provider::C, _) => "c-engine",
            (Provider::Rust, true) => "native-verified",
            (Provider::Rust, false) => "interpreter",
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
        let runner_identity = std::env::current_exe()
            .map_err(|error| format!("runner executable: {error}"))
            .and_then(|path| file_identity(&path))?;
        let options_identity = self.options_identity();
        if let Some(probe) = self.diagnostics_probe() {
            process.sample(&probe)?;
            eprintln!("diagnostic native-proof=ok diagnostics=off-for-timed-samples");
        }
        let mut phases: BTreeMap<String, Summary> = BTreeMap::new();
        let mut walls = Vec::with_capacity(self.repeats);
        for repetition in 0..self.repeats {
            let sample = process.sample(&self)?;
            walls.push(sample.wall);
            if let Some(diagnostics) = sample.x86_diagnostics {
                eprint!(
                    "diagnostic repeat={} x86_public_exits={} x86_public_syscalls={} x86_public_epochs={} x86_syscall_vector_dirty={}",
                    repetition + 1,
                    diagnostics.public_exits,
                    diagnostics.public_syscalls,
                    diagnostics.public_epochs,
                    diagnostics.syscall_vector_dirty,
                );
                if let Some(share) = diagnostics.dirty_share_ppm() {
                    eprint!(" x86_syscall_vector_dirty_ppm={share}");
                }
                eprintln!();
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
                if name != "syscall" && summary.checksum.is_some_and(|expected| expected != phase.checksum) {
                    return Err(format!("checksum changed across repeats for {name}"));
                }
                summary.checksum = Some(phase.checksum);
                summary.times.push(phase.time);
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
        for (name, value) in &self.engine_options {
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

#[cfg(target_os = "linux")]
fn host_snapshot() -> String {
    let load = std::fs::read_to_string("/proc/loadavg").ok().map_or_else(
        || "unknown".into(),
        |value| value.split_whitespace().take(3).collect::<Vec<_>>().join("/"),
    );
    let memory = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        memory
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map_or("unknown", str::trim)
    };
    format!(
        "arch={} cpu_affinity={} load_1_5_15={} mem_available={} swap_free={}",
        std::env::consts::ARCH,
        host_affinity(),
        load,
        field("MemAvailable:"),
        field("SwapFree:"),
    )
}

#[cfg(not(target_os = "linux"))]
fn host_snapshot() -> String {
    format!("arch={} cpu_affinity={}", std::env::consts::ARCH, host_affinity())
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
        Ok(Some((name.into(), Self { time, checksum })))
    }
}

impl Summary {
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
mod test {
    use super::{Isa, Phase, Provider, Run, Summary, adapter};
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn command_run(provider: Provider, binary: PathBuf, engine: Option<PathBuf>) -> Run {
        Run {
            provider,
            isa: Isa::X86,
            binary,
            rootfs: None,
            engine,
            c_runner: None,
            output: None,
            repeats: 1,
            timeout: Duration::from_secs(1),
            guest: vec!["--phase".into(), "compute".into()],
            environment: vec![("BENCH_SEED".into(), "7".into())],
            engine_options: Vec::new(),
            diagnostics_proven: false,
        }
    }

    #[test]
    fn phase_contract() {
        assert!(matches!(
            Phase::parse("PHASE compute us=42 ok=7"),
            Ok(Some((name, Phase { time: 42, checksum: 7 }))) if name == "compute"
        ));
        assert!(Phase::parse("PHASE file us=100 ok=0").is_err());
    }

    #[test]
    fn median_contract() {
        assert_eq!(Summary::median(&mut [9, 1, 5]), 5);
        assert_eq!(Summary::median(&mut [9, 1, 5, 3]), 4);
    }

    #[test]
    fn diagnostic_proof() {
        let run = Run {
            provider: Provider::Rust,
            isa: Isa::Aarch64,
            binary: "/bin/sh".into(),
            rootfs: None,
            engine: Some("/bin/sh".into()),
            c_runner: None,
            output: None,
            repeats: 5,
            timeout: Duration::from_secs(120),
            guest: Vec::new(),
            environment: Vec::new(),
            engine_options: vec![("HL_NATIVE_EXECUTION".into(), "1".into())],
            diagnostics_proven: false,
        }
        .validate()
        .unwrap();
        assert!(run.native_requested());
        assert!(!run.native_diagnostics_requested());
        assert_eq!(run.execution_mode(), "native-verified");
        assert_eq!(run.diagnostics_mode(), "off");
        assert!(
            !run.engine_options
                .iter()
                .any(|(name, _)| name == "HL_NATIVE_DIAGNOSTICS")
        );
        let probe = run.diagnostics_probe().expect("native run proves itself");
        assert!(probe.native_diagnostics_requested());
        assert_eq!(probe.repeats, 1);
        assert!(probe.diagnostics_probe().is_none());
        let command = adapter::Process::new(None).command(&probe).unwrap();
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            arguments
                .windows(2)
                .any(|pair| { pair == ["--engine-option", "HL_NATIVE_EXECUTION=1"] })
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| { pair == ["--engine-option", "HL_NATIVE_DIAGNOSTICS=1"] })
        );
    }

    #[test]
    fn native_settings_are_rejected_where_they_would_not_apply() {
        let mut run = command_run(Provider::Rust, "/bin/sh".into(), Some("/bin/sh".into()));
        run.environment = vec![("HL_NATIVE_EXECUTION".into(), "1".into())];
        assert_eq!(
            run.validate().unwrap_err(),
            "HL_NATIVE_EXECUTION is honoured only as --engine-option, not --env"
        );
    }

    #[test]
    fn provider_commands() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let binary = directory.path().join("guest");
        let engine = directory.path().join("engine");
        fs::write(&binary, []).expect("guest fixture");
        fs::write(&engine, []).expect("engine fixture");

        let process = adapter::Process::new(None);
        let native = process
            .command(&command_run(Provider::Native, binary.clone(), None))
            .expect("native command");
        assert_eq!(native.get_program(), binary.as_os_str());
        assert_eq!(
            native.get_args().collect::<Vec<_>>(),
            [OsStr::new("--phase"), OsStr::new("compute")]
        );
        assert!(
            native
                .get_envs()
                .any(|(name, value)| { name == OsStr::new("BENCH_SEED") && value == Some(OsStr::new("7")) })
        );

        let qemu = process
            .command(&command_run(Provider::Qemu, binary.clone(), None))
            .expect("qemu command");
        assert_eq!(qemu.get_program(), OsStr::new("qemu-x86_64"));
        assert_eq!(
            qemu.get_args().collect::<Vec<_>>(),
            [binary.as_os_str(), OsStr::new("--phase"), OsStr::new("compute")]
        );

        let c = process
            .command(&command_run(Provider::C, binary.clone(), Some(engine.clone())))
            .expect("C engine command");
        assert_eq!(c.get_program(), engine.as_os_str());
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            [binary.as_os_str(), OsStr::new("--phase"), OsStr::new("compute")]
        );

        let wrapper = directory.path().join("runner");
        fs::write(&wrapper, b"prefix ENGINE GUEST [args...] suffix").unwrap();
        let mut wrapped = command_run(Provider::C, binary.clone(), Some(engine.clone()));
        wrapped.c_runner = Some(wrapper.clone());
        let command = process.command(&wrapped).expect("typed C runner command");
        assert_eq!(command.get_program(), wrapper.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                engine.as_os_str(),
                binary.as_os_str(),
                OsStr::new("--phase"),
                OsStr::new("compute"),
            ]
        );

        let rejected = command_run(Provider::C, binary, Some(wrapper));
        assert!(
            process
                .command(&rejected)
                .unwrap_err()
                .contains("exec wrapper configured as --engine")
        );
    }

    #[test]
    fn providers_project_one_rootfs_without_host_execution_shortcuts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let rootfs = directory.path().join("rootfs");
        let binary = rootfs.join("bin/true");
        let engine = directory.path().join("engine");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, []).unwrap();
        fs::write(&engine, []).unwrap();
        let process = adapter::Process::new(None);
        let rooted = |provider, selected_engine| {
            let mut run = command_run(provider, PathBuf::from("/bin/true"), selected_engine);
            run.rootfs = Some(rootfs.clone());
            run.validate().unwrap()
        };
        let native = process.command(&rooted(Provider::Native, None)).unwrap();
        assert_eq!(native.get_program(), binary.as_os_str());
        let qemu = process.command(&rooted(Provider::Qemu, None)).unwrap();
        assert_eq!(
            qemu.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("-L"),
                rootfs.as_os_str(),
                binary.as_os_str(),
                OsStr::new("--phase"),
                OsStr::new("compute")
            ]
        );
        for provider in [Provider::C, Provider::Rust] {
            let command = process.command(&rooted(provider, Some(engine.clone()))).unwrap();
            let arguments = command.get_args().collect::<Vec<_>>();
            assert!(
                arguments
                    .windows(2)
                    .any(|pair| pair == [OsStr::new("--rootfs"), rootfs.as_os_str()])
            );
            assert!(arguments.iter().any(|argument| *argument == OsStr::new("/bin/true")));
        }
        let wrapper = directory.path().join("runner");
        fs::write(&wrapper, b"ENGINE GUEST [args...]").unwrap();
        let mut wrapped = rooted(Provider::C, Some(engine.clone()));
        wrapped.c_runner = Some(wrapper.clone());
        let command = process.command(&wrapped).unwrap();
        assert_eq!(command.get_program(), wrapper.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                engine.as_os_str(),
                OsStr::new("--rootfs"),
                rootfs.as_os_str(),
                OsStr::new("/bin/true"),
                OsStr::new("--phase"),
                OsStr::new("compute")
            ]
        );
    }

    #[test]
    fn rootfs_rejects_unconfined_guest_paths() {
        let directory = tempfile::tempdir().unwrap();
        let mut run = command_run(Provider::Native, PathBuf::from("bin/true"), None);
        run.rootfs = Some(directory.path().to_path_buf());
        assert_eq!(
            run.validate().unwrap_err(),
            "rootfs guest executable must be an absolute confined path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_contract() {
        let run = Run {
            provider: Provider::Native,
            isa: Isa::Aarch64,
            binary: PathBuf::from("/bin/sh"),
            rootfs: None,
            engine: None,
            c_runner: None,
            output: None,
            repeats: 1,
            timeout: Duration::from_secs(1),
            guest: vec!["-c".into(), "sleep 30 & echo 'PHASE wait us=1 ok=1'; wait".into()],
            environment: Vec::new(),
            engine_options: Vec::new(),
            diagnostics_proven: false,
        };
        let started = Instant::now();
        assert!(adapter::Process::new(None).sample(&run).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
