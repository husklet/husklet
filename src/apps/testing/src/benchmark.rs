use clap::{Args, Subcommand, ValueEnum};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

mod adapter;
mod matrix;
pub(crate) mod report;

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

#[derive(Args, Debug)]
pub(crate) struct Run {
    #[arg(long, value_enum)]
    provider: Provider,
    #[arg(long = "arch", value_enum)]
    isa: Isa,
    #[arg(long)]
    binary: PathBuf,
    #[arg(long)]
    engine: Option<PathBuf>,
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
    fn validate(mut self) -> Result<Self, String> {
        if self.repeats == 0 || self.repeats > LIMIT {
            return Err(format!("repeats must be between 1 and {LIMIT}"));
        }
        if self.timeout.is_zero() {
            return Err("timeout must be positive".into());
        }
        if matches!(self.provider, Provider::C | Provider::Rust) && self.engine.is_none() {
            return Err("engine provider requires --engine".into());
        }
        if self.provider != Provider::Rust && !self.engine_options.is_empty() {
            return Err("--engine-option is supported only by rust-engine".into());
        }
        let native = self
            .engine_options
            .iter()
            .any(|(name, value)| name == "HL_NATIVE_EXECUTION" && value == "1");
        if native
            && !self
                .engine_options
                .iter()
                .any(|(name, value)| name == "HL_NATIVE_DIAGNOSTICS" && value == "1")
        {
            self.engine_options.push(("HL_NATIVE_DIAGNOSTICS".into(), "1".into()));
        }
        Ok(self)
    }

    fn native_requested(&self) -> bool {
        self.provider == Provider::Rust
            && self
                .engine_options
                .iter()
                .any(|(name, value)| name == "HL_NATIVE_EXECUTION" && value == "1")
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
        let guest_identity = file_identity(&self.binary)?;
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
        let mut phases: BTreeMap<String, Summary> = BTreeMap::new();
        let mut walls = Vec::with_capacity(self.repeats);
        for repetition in 0..self.repeats {
            let sample = process.sample(&self)?;
            walls.push(sample.wall);
            if let Some(diagnostics) = sample.x86_diagnostics {
                eprint!(
                    "diagnostic repeat={} x86_public_exits={} x86_public_syscalls={} x86_syscall_vector_dirty={}",
                    repetition + 1,
                    diagnostics.public_exits,
                    diagnostics.public_syscalls,
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
        writeln!(writer, "env,arch,phase,us,ok,us_min,us_max,repeats,wall_us,execution,guest_sha256,engine_sha256,runner_sha256,options_sha256,cpu_affinity")
            .map_err(|error| error.to_string())?;
        let wall = Summary::median(&mut walls);
        for (name, mut phase) in phases {
            let minimum = *phase.times.iter().min().expect("nonempty phase");
            let maximum = *phase.times.iter().max().expect("nonempty phase");
            let time = Summary::median(&mut phase.times);
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
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
        identity_field(&mut digest, host_affinity().as_bytes());
        hex_digest(digest.finalize())
    }
}

fn identity_field(digest: &mut sha2::Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}

fn file_identity(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("hash {}: {error}", path.display()))?;
    Ok(hex_digest(sha2::Sha256::digest(bytes)))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
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
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .map(|value| value.split_whitespace().take(3).collect::<Vec<_>>().join("/"))
        .unwrap_or_else(|| "unknown".into());
    let memory = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        memory
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
            .unwrap_or("unknown")
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
        Ok(Some((name.into(), Self { time, checksum })))
    }
}

impl Summary {
    fn median(values: &mut [u64]) -> u64 {
        values.sort_unstable();
        if values.len() % 2 == 0 {
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
            engine,
            output: None,
            repeats: 1,
            timeout: Duration::from_secs(1),
            guest: vec!["--phase".into(), "compute".into()],
            environment: vec![("BENCH_SEED".into(), "7".into())],
            engine_options: Vec::new(),
        }
    }

    #[test]
    fn phase_contract() {
        assert!(matches!(
            Phase::parse("PHASE compute us=42 ok=7"),
            Ok(Some((name, Phase { time: 42, checksum: 7 }))) if name == "compute"
        ));
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
            engine: Some("/bin/sh".into()),
            output: None,
            repeats: 5,
            timeout: Duration::from_secs(120),
            guest: Vec::new(),
            environment: Vec::new(),
            engine_options: vec![("HL_NATIVE_EXECUTION".into(), "1".into())],
        }
        .validate()
        .unwrap();
        assert!(run.native_requested());
        assert_eq!(run.execution_mode(), "native-verified");
        assert!(
            run.engine_options
                .iter()
                .any(|(name, value)| name == "HL_NATIVE_DIAGNOSTICS" && value == "1")
        );
        let command = adapter::Process::new(None).command(&run).unwrap();
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
    }

    #[cfg(unix)]
    #[test]
    fn timeout_contract() {
        let run = Run {
            provider: Provider::Native,
            isa: Isa::Aarch64,
            binary: PathBuf::from("/bin/sh"),
            engine: None,
            output: None,
            repeats: 1,
            timeout: Duration::from_secs(1),
            guest: vec!["-c".into(), "sleep 30 & echo 'PHASE wait us=1 ok=1'; wait".into()],
            environment: Vec::new(),
            engine_options: Vec::new(),
        };
        let started = Instant::now();
        assert!(adapter::Process::new(None).sample(&run).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
