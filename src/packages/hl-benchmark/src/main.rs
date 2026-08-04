use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

mod adapter;
mod report;

const LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Isa {
    Aarch64,
    X86,
}

impl Isa {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "aarch64" | "arm64" => Ok(Self::Aarch64),
            "x86_64" | "amd64" => Ok(Self::X86),
            _ => Err(format!("unsupported architecture: {value}")),
        }
    }

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provider {
    Native,
    Qemu,
    C,
    Rust,
}

impl Provider {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "native" => Ok(Self::Native),
            "qemu" => Ok(Self::Qemu),
            "c-engine" => Ok(Self::C),
            "rust-engine" => Ok(Self::Rust),
            _ => Err(format!("unsupported provider: {value}")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Qemu => "qemu",
            Self::C => "c-engine",
            Self::Rust => "rust-engine",
        }
    }
}

#[derive(Debug)]
struct Run {
    provider: Provider,
    isa: Isa,
    binary: PathBuf,
    engine: Option<PathBuf>,
    output: Option<PathBuf>,
    repeats: usize,
    timeout: Duration,
    guest: Vec<String>,
    environment: Vec<(String, String)>,
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
}

#[derive(Default)]
struct Summary {
    checksum: Option<u64>,
    times: Vec<u64>,
}

fn main() -> ExitCode {
    match execute(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hl-benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

#[hl_design::classify(pkg)]
fn execute(arguments: Vec<String>) -> Result<(), String> {
    let Some(command) = arguments.first() else {
        return Err(usage());
    };
    match command.as_str() {
        "run" => run(parse_run(&arguments[1..])?),
        "report" => report::run(&arguments[1..]),
        "list" => list(&arguments[1..]),
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage: hl-benchmark run --provider native|qemu|c-engine|rust-engine --arch arm64|amd64 \
     --binary GUEST [--engine ENGINE] [--repeats N] [--timeout SECONDS] [--out CSV] \
     [--env KEY=VALUE] [--engine-option KEY=VALUE] [-- GUEST_ARGUMENT ...]\n       hl-benchmark list --arch ISA --binary GUEST \
     [--c-engine PATH] [--rust-engine PATH]\n       hl-benchmark report [--baseline PROVIDER] CSV ..."
        .into()
}

#[hl_design::classify(pkg)]
fn value(arguments: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    *index += 1;
    arguments
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("{name} requires a value"))
}

#[hl_design::classify(pkg)]
fn parse_run(arguments: &[String]) -> Result<Run, String> {
    let mut provider = None;
    let mut isa = None;
    let mut binary = None;
    let mut engine = None;
    let mut output = None;
    let mut repeats = 5;
    let mut timeout = Duration::from_secs(120);
    let mut environment = Vec::new();
    let mut engine_options = Vec::new();
    let mut guest = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--" => {
                guest.extend(arguments[index + 1..].iter().cloned());
                break;
            }
            "--provider" => provider = Some(Provider::parse(&value(arguments, &mut index, "--provider")?)?),
            "--arch" => isa = Some(Isa::parse(&value(arguments, &mut index, "--arch")?)?),
            "--binary" => binary = Some(PathBuf::from(value(arguments, &mut index, "--binary")?)),
            "--engine" => engine = Some(PathBuf::from(value(arguments, &mut index, "--engine")?)),
            "--out" => output = Some(PathBuf::from(value(arguments, &mut index, "--out")?)),
            "--repeats" => {
                repeats = value(arguments, &mut index, "--repeats")?
                    .parse()
                    .map_err(|_| "invalid repeat count".to_string())?;
            }
            "--timeout" => {
                let seconds = value(arguments, &mut index, "--timeout")?
                    .parse()
                    .map_err(|_| "invalid timeout".to_string())?;
                timeout = Duration::from_secs(seconds);
            }
            "--env" => {
                let assignment = value(arguments, &mut index, "--env")?;
                let (name, value) = assignment
                    .split_once('=')
                    .ok_or_else(|| "--env requires KEY=VALUE".to_string())?;
                if name.is_empty() {
                    return Err("environment name cannot be empty".into());
                }
                environment.push((name.into(), value.into()));
            }
            "--engine-option" => {
                let assignment = value(arguments, &mut index, "--engine-option")?;
                let (name, value) = assignment
                    .split_once('=')
                    .ok_or_else(|| "--engine-option requires KEY=VALUE".to_string())?;
                if name.is_empty() {
                    return Err("engine option name cannot be empty".into());
                }
                engine_options.push((name.into(), value.into()));
            }
            option => return Err(format!("unknown option: {option}")),
        }
        index += 1;
    }
    if repeats == 0 || repeats > LIMIT {
        return Err(format!("repeats must be between 1 and {LIMIT}"));
    }
    if timeout.is_zero() {
        return Err("timeout must be positive".into());
    }
    let provider = provider.ok_or_else(|| "--provider is required".to_string())?;
    if matches!(provider, Provider::C | Provider::Rust) && engine.is_none() {
        return Err("engine provider requires --engine".into());
    }
    if provider != Provider::Rust && !engine_options.is_empty() {
        return Err("--engine-option is supported only by rust-engine".into());
    }
    let native = engine_options
        .iter()
        .any(|(name, value)| name == "HL_NATIVE_EXECUTION" && value == "1");
    if native
        && !engine_options
            .iter()
            .any(|(name, value)| name == "HL_NATIVE_DIAGNOSTICS" && value == "1")
    {
        engine_options.push(("HL_NATIVE_DIAGNOSTICS".into(), "1".into()));
    }
    Ok(Run {
        provider,
        isa: isa.ok_or_else(|| "--arch is required".to_string())?,
        binary: binary.ok_or_else(|| "--binary is required".to_string())?,
        engine,
        output,
        repeats,
        timeout,
        guest,
        environment,
        engine_options,
    })
}

impl Run {
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
}

#[hl_design::classify(pkg)]
fn list(arguments: &[String]) -> Result<(), String> {
    let mut isa = None;
    let mut binary = None;
    let mut c_engine = None;
    let mut rust_engine = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--arch" => isa = Some(Isa::parse(&value(arguments, &mut index, "--arch")?)?),
            "--binary" => binary = Some(PathBuf::from(value(arguments, &mut index, "--binary")?)),
            "--c-engine" => c_engine = Some(PathBuf::from(value(arguments, &mut index, "--c-engine")?)),
            "--rust-engine" => rust_engine = Some(PathBuf::from(value(arguments, &mut index, "--rust-engine")?)),
            option => return Err(format!("unknown option: {option}")),
        }
        index += 1;
    }
    let isa = isa.ok_or_else(|| "--arch is required".to_string())?;
    let binary = binary.ok_or_else(|| "--binary is required".to_string())?;
    let host = std::env::consts::ARCH;
    let native = matches!((host, isa), ("aarch64", Isa::Aarch64) | ("x86_64", Isa::X86));
    let rows = [
        ("native", adapter::executable(&binary) && native, format!("host={host}")),
        (
            "qemu",
            adapter::executable(&binary) && adapter::available(&format!("qemu-{}", isa.name())),
            format!("qemu-{}", isa.name()),
        ),
        (
            "c-engine",
            adapter::executable(&binary) && c_engine.as_deref().is_some_and(adapter::executable),
            c_engine.map_or_else(|| "not configured".into(), |path| path.display().to_string()),
        ),
        (
            "rust-engine",
            adapter::executable(&binary) && rust_engine.as_deref().is_some_and(adapter::executable),
            rust_engine.map_or_else(|| "not configured".into(), |path| path.display().to_string()),
        ),
    ];
    println!("provider,arch,reachable,detail");
    for (provider, reachable, detail) in rows {
        println!("{provider},{},{reachable},{detail}", isa.public());
    }
    Ok(())
}

#[hl_design::classify(pkg)]
fn parse_phase(line: &str) -> Result<Option<(String, Phase)>, String> {
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
    Ok(Some((name.into(), Phase { time, checksum })))
}

#[hl_design::classify(pkg)]
fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    if values.len() % 2 == 0 {
        values[values.len() / 2 - 1].saturating_add(values[values.len() / 2]) / 2
    } else {
        values[values.len() / 2]
    }
}

#[hl_design::classify(pkg)]
fn run(run: Run) -> Result<(), String> {
    let mut phases: BTreeMap<String, Summary> = BTreeMap::new();
    let mut walls = Vec::with_capacity(run.repeats);
    for repetition in 0..run.repeats {
        let sample = adapter::sample(&run)?;
        walls.push(sample.wall);
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
    if phases.values().any(|phase| phase.times.len() != run.repeats) {
        return Err("phase set changed across repeats".into());
    }
    let mut writer: Box<dyn Write> = match &run.output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("output directory: {error}"))?;
            }
            Box::new(File::create(path).map_err(|error| format!("output file: {error}"))?)
        }
        None => Box::new(io::stdout()),
    };
    writeln!(writer, "env,arch,phase,us,ok,us_min,us_max,repeats,wall_us,execution")
        .map_err(|error| error.to_string())?;
    let wall = median(&mut walls);
    for (name, mut phase) in phases {
        let minimum = *phase.times.iter().min().expect("nonempty phase");
        let maximum = *phase.times.iter().max().expect("nonempty phase");
        let time = median(&mut phase.times);
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{}",
            run.provider.name(),
            run.isa.public(),
            name,
            time,
            phase.checksum.expect("validated checksum"),
            minimum,
            maximum,
            run.repeats,
            wall,
            run.execution_mode(),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::{Isa, Phase, Provider, Run, adapter, median, parse_phase, parse_run};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn phase_contract() {
        assert!(matches!(
            parse_phase("PHASE compute us=42 ok=7"),
            Ok(Some((name, Phase { time: 42, checksum: 7 }))) if name == "compute"
        ));
    }

    #[test]
    fn median_contract() {
        assert_eq!(median(&mut [9, 1, 5]), 5);
        assert_eq!(median(&mut [9, 1, 5, 3]), 4);
    }

    #[test]
    fn native_option_enables_diagnostic_proof() {
        let run = parse_run(&[
            "--provider".into(),
            "rust-engine".into(),
            "--arch".into(),
            "arm64".into(),
            "--binary".into(),
            "/bin/sh".into(),
            "--engine".into(),
            "/bin/sh".into(),
            "--engine-option".into(),
            "HL_NATIVE_EXECUTION=1".into(),
        ])
        .unwrap();
        assert!(run.native_requested());
        assert_eq!(run.execution_mode(), "native-verified");
        assert!(
            run.engine_options
                .iter()
                .any(|(name, value)| name == "HL_NATIVE_DIAGNOSTICS" && value == "1")
        );
        let command = adapter::command(&run).unwrap();
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
        assert!(adapter::sample(&run).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
