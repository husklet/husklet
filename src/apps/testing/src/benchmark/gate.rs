//! Authoritative Rust-vs-retained-C verdict: harness-enforced wiring and CPU
//! pinning, median statistics with a noise floor, and a committed baseline gate.

use super::{Isa, LIMIT, matrix::Matrix, parse_duration, workload::Workload};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PINNED: &str = "HL_BENCH_PINNED";
const PROVIDERS: [&str; 3] = ["native", "c-engine", "rust-engine"];

#[derive(Args, Debug)]
pub(crate) struct Gate {
    #[arg(long, value_enum)]
    workload: Workload,
    #[arg(long = "arch", value_enum)]
    isa: Isa,
    #[arg(long)]
    binary: PathBuf,
    /// Retained C build root; the harness selects the engine and its exec wrapper.
    #[arg(long = "c-build")]
    c_build: PathBuf,
    #[arg(long)]
    rust_engine: PathBuf,
    #[arg(long, default_value = "src/apps/testing/benchmark-baseline.tsv")]
    baseline: PathBuf,
    #[arg(long = "out", default_value = "target/testing/benchmark-gate")]
    output: PathBuf,
    #[arg(long, default_value_t = 7)]
    repeats: usize,
    #[arg(long, default_value_t = 1)]
    divisor: u32,
    /// Refuse a verdict when any measured series spreads wider than this fraction.
    #[arg(long, default_value_t = 0.05)]
    max_spread: f64,
    /// Rust regression allowance above the committed baseline.
    #[arg(long, default_value_t = 0.05)]
    tolerance: f64,
    /// Rewrite the committed baseline from this run instead of judging it.
    #[arg(long)]
    update: bool,
    /// CPU the harness pins every provider to; defaults to a PID-derived slot in the inherited affinity.
    #[arg(long)]
    cpu: Option<usize>,
    #[arg(long, default_value = "600", value_parser = parse_duration)]
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Statistics {
    pub median: u64,
    pub minimum: u64,
    pub maximum: u64,
    quartiles: (u64, u64),
}

/// A committed baseline: the builds it was measured against and its samples.
#[derive(Default)]
pub(crate) struct Baseline {
    engines: BTreeMap<String, String>,
    revision: Option<String>,
    samples: BTreeMap<(String, String, String), u64>,
}

impl Statistics {
    pub(crate) fn of(values: &[u64]) -> Option<Self> {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        let median = *sorted.get(sorted.len() / 2)?;
        Some(Self {
            median,
            minimum: sorted[0],
            maximum: sorted[sorted.len() - 1],
            quartiles: (sorted[sorted.len() / 4], sorted[sorted.len() * 3 / 4]),
        })
    }

    /// Interquartile width over the median, so one intruding process cannot
    /// veto a run while sustained contention still does.
    pub(crate) fn spread(self) -> f64 {
        if self.median == 0 {
            return f64::INFINITY;
        }
        ratio(self.quartiles.1 - self.quartiles.0, self.median)
    }
}

impl Baseline {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut baseline = Self::default();
        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let fields = line.split('\t').collect::<Vec<_>>();
            match fields.as_slice() {
                ["engine", "c-engine", arch, build_id] => {
                    baseline.engines.insert((*arch).to_owned(), (*build_id).to_owned());
                }
                ["engine", "rust-engine", revision] => baseline.revision = Some((*revision).to_owned()),
                ["sample", arch, workload, phase, us] => {
                    let value = us.parse().map_err(|_| format!("invalid baseline sample: {line}"))?;
                    baseline
                        .samples
                        .insert(((*arch).to_owned(), (*workload).to_owned(), (*phase).to_owned()), value);
                }
                _ => return Err(format!("invalid baseline row: {line}")),
            }
        }
        Ok(baseline)
    }

    /// Replaces only this arch and workload's rows so other records survive.
    fn render(
        &self,
        provenance: &Provenance,
        arch: &str,
        workload: &str,
        samples: &BTreeMap<String, Statistics>,
    ) -> String {
        let mut merged = self.samples.clone();
        merged.retain(|(recorded_arch, recorded, _), _| recorded_arch != arch || recorded != workload);
        for (phase, statistics) in samples {
            merged.insert((arch.to_owned(), workload.to_owned(), phase.clone()), statistics.median);
        }
        let mut engines = self.engines.clone();
        engines.insert(arch.to_owned(), provenance.build_id.clone());
        let mut text = String::from("# husklet benchmark baseline v1: rust-engine median microseconds\n");
        for (arch, build_id) in &engines {
            text.push_str(&format!("engine\tc-engine\t{arch}\t{build_id}\n"));
        }
        text.push_str(&format!("engine\trust-engine\t{}\n", provenance.revision));
        for ((arch, workload, phase), us) in &merged {
            text.push_str(&format!("sample\t{arch}\t{workload}\t{phase}\t{us}\n"));
        }
        text
    }
}

/// Everything a quoted number must be traceable to.
pub(crate) struct Provenance {
    build_id: String,
    revision: String,
    rust_sha256: String,
    host_load: String,
}

impl Provenance {
    fn dirty(&self) -> bool {
        self.revision.ends_with("-dirty")
    }

    fn print(&self, workload: &str, cpu: usize, repeats: usize) {
        println!(
            "provenance\tworkload={workload}\trust_revision={}\trust_sha256={}\tc_build_id={}\tcpu={cpu}\trepeats={repeats}\thost_load={}",
            self.revision, self.rust_sha256, self.build_id, self.host_load
        );
    }
}

/// The `hl-engine` binary is produced by the `engine` package, so `cargo build -p hl-engine`
/// builds only the library and leaves whatever binary a previous or foreign build left behind.
pub(crate) const ENGINE_BUILD: [&str; 7] = ["build", "--release", "--locked", "-p", "engine", "--bin", "hl-engine"];

/// Builds the engine binary and reports where cargo put it, so the gate measures this tree's
/// binary instead of trusting a path another lane may have written.
fn build_engine() -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(ENGINE_BUILD)
        .arg("--message-format=json-render-diagnostics")
        .output()
        .map_err(|error| format!("build the engine binary: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo {} failed:\n{}",
            ENGINE_BUILD.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    artifact(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("cargo {} produced no hl-engine executable", ENGINE_BUILD.join(" ")))
}

/// Extracts the `hl-engine` executable path from a cargo JSON artifact stream.
fn artifact(stream: &str) -> Option<PathBuf> {
    stream
        .lines()
        .filter_map(|line| line.split("\"executable\":\"").nth(1))
        .filter_map(|tail| tail.split('"').next())
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .find(|path| path.file_name().is_some_and(|name| name == "hl-engine"))
}

/// The source revision the Rust engine under test was built from.
pub(crate) fn revision() -> String {
    let git = |arguments: &[&str]| {
        std::process::Command::new("git")
            .args(arguments)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(head) = git(&["rev-parse", "--short=12", "HEAD"]).filter(|head| !head.is_empty()) else {
        return "unknown".into();
    };
    match git(&["status", "--porcelain"]) {
        Some(status) if status.is_empty() => head,
        _ => format!("{head}-dirty"),
    }
}

fn ratio(value: u64, reference: u64) -> f64 {
    if reference == 0 {
        f64::INFINITY
    } else {
        value as f64 / reference as f64
    }
}

/// Parses `Cpus_allowed_list` and reports the CPU the run must be pinned to.
pub(crate) fn pinning(affinity: &str, requested: Option<usize>) -> Result<(usize, bool), String> {
    slot(affinity, requested, std::process::id() as usize)
}

/// Spreads defaulted pins across the allowed set by `seed`, so concurrent lanes
/// do not all land on the same core.
fn slot(affinity: &str, requested: Option<usize>, seed: usize) -> Result<(usize, bool), String> {
    let mut allowed = Vec::new();
    for span in affinity.trim().split(',') {
        let (start, end) = span.split_once('-').unwrap_or((span, span));
        let start = start.trim().parse::<usize>().map_err(|_| "unreadable CPU affinity")?;
        let end = end.trim().parse::<usize>().map_err(|_| "unreadable CPU affinity")?;
        if end < start {
            return Err("unreadable CPU affinity".into());
        }
        allowed.extend(start..=end);
    }
    if allowed.is_empty() {
        return Err("unreadable CPU affinity".into());
    }
    // CPU 0 carries most IRQ work, so defaults avoid it whenever anything else is allowed.
    let pool = if allowed.len() > 1 && allowed[0] == 0 { &allowed[1..] } else { &allowed[..] };
    let cpu = requested.unwrap_or_else(|| pool[seed % pool.len()]);
    if !allowed.contains(&cpu) {
        return Err(format!("CPU {cpu} is outside the inherited affinity {affinity}"));
    }
    Ok((cpu, allowed.as_slice() == [cpu]))
}

/// The guest lowering a run actually exercises, so nobody proves an x86-64
/// change by measuring an ARM64 guest.
pub(crate) const fn lowering(isa: Isa) -> &'static str {
    match isa {
        Isa::Aarch64 => "src/native/exec/src/arch/aarch64",
        Isa::X86 => "src/native/exec/src/arch/x86_64",
    }
}

/// Names every prerequisite a lane must build before the gate can measure.
pub(crate) fn missing(guest: &Path, rust_engine: &Path, c_engine: &Path, runner: &Path, arch: &str) -> Vec<String> {
    let mut missing = Vec::new();
    if !guest.is_file() {
        missing.push(format!(
            "guest {} is missing: make bench-guest BENCH_ARCH={arch}",
            guest.display()
        ));
    }
    if !rust_engine.is_file() {
        missing.push(format!(
            "rust engine {} is missing: cargo {}",
            rust_engine.display(),
            ENGINE_BUILD.join(" ")
        ));
    }
    if !c_engine.is_file() {
        missing.push(format!(
            "retained C engine {} is missing: build it in the engine tree and pass --c-build",
            c_engine.display()
        ));
    }
    if !runner.is_file() {
        missing.push(format!("retained C exec wrapper {} is missing", runner.display()));
    }
    missing
}

/// Resolves the retained engine and its exec wrapper from a build root.
pub(crate) fn wiring(root: &Path, isa: Isa) -> (PathBuf, PathBuf) {
    (
        root.join("linux-production")
            .join(format!("hl-engine-linux-{}", isa.name())),
        root.join("bin").join("hl-engine-runner"),
    )
}

impl Gate {
    pub(super) fn execute(self, process: &super::adapter::Process) -> Result<(), String> {
        // Before the repin, so a cold engine build does not run on the single pinned CPU.
        build_engine()?;
        if let Some(status) = self.repin()? {
            std::process::exit(status);
        }
        if self.repeats < 3 || self.repeats > LIMIT {
            return Err(format!("gate repeats must be between 3 and {LIMIT}"));
        }
        let (engine, runner) = wiring(&self.c_build, self.isa);
        println!(
            "coverage\tguest_arch={}\tlowering={}\thost_arch={}",
            self.isa.public(),
            lowering(self.isa),
            std::env::consts::ARCH
        );
        let foreign = !matches!(
            (std::env::consts::ARCH, self.isa),
            ("aarch64", Isa::Aarch64) | ("x86_64", Isa::X86)
        );
        if foreign {
            println!(
                "coverage\tnative_baseline=absent\treason=a {} host cannot run a {} guest directly",
                std::env::consts::ARCH,
                self.isa.public()
            );
        }
        let built = build_engine()?;
        let missing = missing(&self.binary, &self.rust_engine, &engine, &runner, self.isa.public());
        if !missing.is_empty() {
            return Err(format!("gate prerequisites are not built:\n  {}", missing.join("\n  ")));
        }
        let rust_sha256 = super::file_identity(&self.rust_engine)?;
        if rust_sha256 != super::file_identity(&built)? {
            return Err(format!(
                "--rust-engine {} is not the binary this tree builds ({}); it is stale or from another lane",
                self.rust_engine.display(),
                built.display()
            ));
        }
        let provenance = Provenance {
            build_id: super::matrix::build_identity(&engine)?,
            revision: revision(),
            rust_sha256,
            host_load: crate::runtime::load::sample(),
        };
        let (cpu, _) = pinning(&super::host_affinity(), self.cpu)?;
        provenance.print(self.workload.name(), cpu, self.repeats);
        let baseline = self.baseline()?;
        if let Some(pinned) = baseline.engines.get(self.isa.public()).map(String::as_str)
            && !self.update
            && !pinned.eq_ignore_ascii_case(&provenance.build_id)
        {
            return Err(format!(
                "retained C build changed: baseline pins {pinned}, {} has {}; re-record with --update",
                engine.display(),
                provenance.build_id
            ));
        }
        if self.update && provenance.dirty() {
            return Err("refusing to record a baseline from a dirty tree; commit first".into());
        }
        let paths = self.measure(process, engine, runner, &provenance.build_id, foreign)?;
        let series = collect(&paths)?;
        self.verdict(&series, &provenance, &baseline)
    }

    /// Re-executes under `taskset` when the inherited affinity is not one CPU.
    fn repin(&self) -> Result<Option<i32>, String> {
        let (cpu, pinned) = pinning(&super::host_affinity(), self.cpu)?;
        if pinned || std::env::var_os(PINNED).is_some() {
            return Ok(None);
        }
        let executable = std::env::current_exe().map_err(|error| format!("runner executable: {error}"))?;
        let status = std::process::Command::new("taskset")
            .arg("-c")
            .arg(cpu.to_string())
            .arg(executable)
            .args(std::env::args_os().skip(1))
            .env(PINNED, "1")
            .status()
            .map_err(|error| format!("pin benchmark to CPU {cpu}: {error}"))?;
        Ok(Some(status.code().unwrap_or(1)))
    }

    fn baseline(&self) -> Result<Baseline, String> {
        match std::fs::read_to_string(&self.baseline) {
            Ok(text) => Baseline::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Baseline::default()),
            Err(error) => Err(format!("baseline {}: {error}", self.baseline.display())),
        }
    }

    fn measure(
        &self,
        process: &super::adapter::Process,
        engine: PathBuf,
        runner: PathBuf,
        build_id: &str,
        skip_native: bool,
    ) -> Result<Vec<PathBuf>, String> {
        let matrix = Matrix::gated(
            self.isa,
            self.binary.clone(),
            engine,
            runner,
            self.c_build.display().to_string(),
            build_id.to_owned(),
            self.rust_engine.clone(),
            self.output.join(self.workload.name()),
            self.repeats,
            self.timeout,
            self.workload.guest(self.divisor),
            skip_native,
        );
        matrix.validate()?.produce(process)
    }

    fn verdict(&self, series: &Series, provenance: &Provenance, baseline: &Baseline) -> Result<(), String> {
        let mut summaries: BTreeMap<String, BTreeMap<&str, Statistics>> = BTreeMap::new();
        for ((phase, provider), values) in series {
            let statistics = Statistics::of(values).ok_or_else(|| format!("no samples for {phase}/{provider}"))?;
            let provider = PROVIDERS
                .iter()
                .find(|candidate| *candidate == provider)
                .ok_or_else(|| format!("unknown provider {provider}"))?;
            summaries.entry(phase.clone()).or_default().insert(provider, statistics);
        }
        if summaries.is_empty() {
            return Err("gate measured no phases".into());
        }
        println!(
            "workload\tphase\tnative_us\tc_us\trust_us\trust_min_us\trust_max_us\trust_x_c\trust_x_native\tspread\tbaseline_x"
        );
        let mut noisy = Vec::new();
        let mut regressed = Vec::new();
        let mut rust = BTreeMap::new();
        for (phase, providers) in &summaries {
            let pick = |name: &str| providers.get(name).copied();
            let (native, Some(c), Some(engine)) = (pick("native"), pick("c-engine"), pick("rust-engine")) else {
                return Err(format!("phase {phase} is missing an engine row"));
            };
            let spread = native
                .into_iter()
                .chain([c, engine])
                .map(Statistics::spread)
                .fold(0.0, f64::max);
            if spread > self.max_spread {
                noisy.push(format!("{phase} spread {spread:.3}"));
            }
            let recorded = baseline.samples.get(&(
                self.isa.public().to_owned(),
                self.workload.name().to_owned(),
                phase.clone(),
            ));
            let against = recorded.map_or_else(
                || "-".to_owned(),
                |value| format!("{:.3}", ratio(engine.median, *value)),
            );
            println!(
                "{}\t{phase}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{spread:.3}\t{against}",
                self.workload.name(),
                native.map_or_else(|| "-".to_owned(), |value| value.median.to_string()),
                c.median,
                engine.median,
                engine.minimum,
                engine.maximum,
                ratio(engine.median, c.median),
                native.map_or_else(
                    || "-".to_owned(),
                    |value| format!("{:.3}", ratio(engine.median, value.median))
                ),
            );
            if let Some(value) = recorded {
                let allowance = 1.0 + self.tolerance.max(spread);
                if ratio(engine.median, *value) > allowance {
                    regressed.push(format!("{phase} {}us over baseline {value}us", engine.median));
                }
            }
            rust.insert(phase.clone(), engine);
        }
        if !noisy.is_empty() {
            return Err(format!(
                "no verdict: run-to-run spread exceeds {:.3} for {}",
                self.max_spread,
                noisy.join(", ")
            ));
        }
        if self.update {
            let text = baseline.render(provenance, self.isa.public(), self.workload.name(), &rust);
            std::fs::write(&self.baseline, text).map_err(|error| format!("write baseline: {error}"))?;
            println!("baseline\t{}\trecorded", self.baseline.display());
            return Ok(());
        }
        println!(
            "baseline\trevision={}\tcompared_against={}",
            baseline.revision.as_deref().unwrap_or("none"),
            provenance.revision
        );
        if crate::runtime::load::saturated(&provenance.host_load) {
            println!(
                "verdict\tsuspect\thost_load={} is too contended to compare timings against",
                provenance.host_load
            );
        }
        if regressed.is_empty() {
            println!("verdict\tpass\t{} phases within {:.3}", rust.len(), self.tolerance);
            Ok(())
        } else {
            Err(format!("rust-engine regressed: {}", regressed.join(", ")))
        }
    }
}

type Series = BTreeMap<(String, String), Vec<u64>>;

/// Work done per `(phase, provider)`: the summed `ok=` checksum and the rows it came from.
type Totals = BTreeMap<(String, String), (u64, usize)>;

/// The syscall checksum is a sum of thread ids, so it legitimately differs between
/// providers; every other phase counts iterations and must agree across arms.
const UNCOMPARABLE: [&str; 1] = ["syscall"];

fn collect(paths: &[PathBuf]) -> Result<Series, String> {
    let mut series: Series = BTreeMap::new();
    let mut totals: Totals = BTreeMap::new();
    let mut builds: BTreeMap<String, String> = BTreeMap::new();
    for path in paths {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut lines = text.lines();
        let header = lines
            .next()
            .ok_or_else(|| format!("empty row file: {}", path.display()))?;
        let columns = header.split(',').collect::<Vec<_>>();
        let index = |name: &str| {
            columns
                .iter()
                .position(|column| *column == name)
                .ok_or_else(|| format!("{} lacks column {name}", path.display()))
        };
        let (provider, phase, us) = (index("env")?, index("phase")?, index("us")?);
        let (guest, engine, ok) = (index("guest_sha256")?, index("engine_sha256")?, index("ok")?);
        for line in lines {
            let fields = line.split(',').collect::<Vec<_>>();
            let field = |position: usize| {
                fields
                    .get(position)
                    .copied()
                    .ok_or_else(|| "short benchmark row".to_owned())
            };
            let value = field(us)?.parse().map_err(|_| "invalid benchmark time".to_owned())?;
            let identity = format!("{}/{}", field(guest)?, field(engine)?);
            match builds
                .entry(field(provider)?.to_owned())
                .or_insert_with(|| identity.clone())
            {
                recorded if *recorded == identity => {}
                recorded => {
                    return Err(format!(
                        "refusing to compare rows from different trees: {} measured {recorded} and {identity}",
                        field(provider)?
                    ));
                }
            }
            let checksum: u64 = field(ok)?
                .parse()
                .map_err(|_| "invalid benchmark checksum".to_owned())?;
            let key = (field(phase)?.to_owned(), field(provider)?.to_owned());
            let done = totals.entry(key.clone()).or_insert((0, 0));
            *done = (done.0.saturating_add(checksum), done.1 + 1);
            series.entry(key).or_default().push(value);
        }
    }
    identical_work(&totals)?;
    Ok(series)
}

/// Refuses a verdict unless every arm ran every phase to the same completion, so a
/// run that aborted partway cannot be read as a smaller-is-faster win.
fn identical_work(totals: &Totals) -> Result<(), String> {
    let providers = totals.keys().map(|(_, provider)| provider).collect::<BTreeSet<_>>();
    let mut phases: BTreeMap<&String, BTreeMap<&String, (u64, usize)>> = BTreeMap::new();
    for ((phase, provider), done) in totals {
        phases.entry(phase).or_default().insert(provider, *done);
    }
    for (phase, measured) in phases {
        let absent = providers
            .iter()
            .filter(|provider| !measured.contains_key(**provider))
            .map(|provider| provider.as_str())
            .collect::<Vec<_>>();
        if !absent.is_empty() {
            return Err(format!(
                "refusing the verdict: phase {phase} has no rows for {}; that arm stopped before it ran",
                absent.join(", ")
            ));
        }
        if measured.values().map(|(_, rows)| *rows).collect::<BTreeSet<_>>().len() > 1 {
            return Err(format!(
                "refusing the verdict: phase {phase} has unequal sample counts across arms ({}); an arm stopped early",
                describe(&measured, |(_, rows)| format!("n={rows}"))
            ));
        }
        if UNCOMPARABLE.contains(&phase.as_str()) {
            continue;
        }
        if measured.values().map(|(ok, _)| *ok).collect::<BTreeSet<_>>().len() > 1 {
            return Err(format!(
                "refusing the verdict: phase {phase} did unequal work across arms ({}); an arm stopped early and its smaller ok= is not a speedup",
                describe(&measured, |(ok, rows)| format!("ok={ok} over {rows} samples"))
            ));
        }
    }
    Ok(())
}

/// Renders one clause per arm so a refusal names every side of the disagreement.
fn describe(measured: &BTreeMap<&String, (u64, usize)>, render: impl Fn(&(u64, usize)) -> String) -> String {
    measured
        .iter()
        .map(|(provider, done)| format!("{provider} {}", render(done)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{Baseline, ENGINE_BUILD, Isa, Provenance, Statistics, artifact, collect, pinning, revision, slot, wiring};

    fn provenance() -> Provenance {
        Provenance {
            build_id: "abc".into(),
            revision: "0123456789ab".into(),
            rust_sha256: "feed".into(),
            host_load: "0.10/8".into(),
        }
    }

    #[test]
    fn the_engine_binary_is_built_from_its_own_package_not_the_library() {
        let package = ENGINE_BUILD.iter().position(|argument| *argument == "-p").unwrap() + 1;
        assert_eq!(ENGINE_BUILD[package], "engine");
        assert_eq!(ENGINE_BUILD[ENGINE_BUILD.len() - 2..], ["--bin", "hl-engine"]);
    }

    #[test]
    fn the_artifact_stream_yields_only_the_engine_executable() {
        let stream = concat!(
            "{\"reason\":\"compiler-artifact\",\"executable\":null}\n",
            "{\"reason\":\"compiler-artifact\",\"executable\":\"/t/release/hl-aarch64\"}\n",
            "{\"reason\":\"compiler-artifact\",\"executable\":\"/t/release/hl-engine\"}\n",
        );
        assert_eq!(artifact(stream), Some(std::path::PathBuf::from("/t/release/hl-engine")));
        assert_eq!(artifact("{\"reason\":\"build-finished\"}\n"), None);
    }

    #[test]
    fn statistics_report_median_and_spread() {
        let statistics = Statistics::of(&[100, 104, 102]).unwrap();
        assert_eq!(
            (statistics.median, statistics.minimum, statistics.maximum),
            (102, 100, 104)
        );
        assert!((statistics.spread() - 4.0 / 102.0).abs() < 1e-9);
        assert!(Statistics::of(&[]).is_none());
        let outlier = Statistics::of(&[100, 101, 102, 103, 104, 105, 900]).unwrap();
        assert_eq!(outlier.median, 103);
        assert!(outlier.spread() < 0.05);
    }

    #[test]
    fn pinning_selects_one_cpu_and_rejects_foreign_requests() {
        assert_eq!(slot("0-17", None, 4).unwrap(), (5, false));
        assert_eq!(slot("5", None, 12345).unwrap(), (5, true));
        assert_eq!(slot("0-3,8", Some(8), 0).unwrap(), (8, false));
        assert!(slot("0-3", Some(9), 0).is_err());
        assert!(slot("unknown", None, 0).is_err());
        assert!(pinning("0-17", None).unwrap().0 <= 17);
    }

    #[test]
    fn defaulted_pins_spread_across_the_allowed_set() {
        let pins = (1000..1017)
            .map(|seed| slot("0-17", None, seed).unwrap().0)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(pins.len(), 17);
        assert!(!pins.contains(&0));
        assert_eq!(slot("0", None, 3).unwrap(), (0, true));
        assert_eq!(slot("0-3,8", None, 4).unwrap().0, 1);
        // An explicit request still wins whatever the seed is.
        assert_eq!(slot("0-17", Some(12), 999).unwrap().0, 12);
    }

    #[test]
    fn each_guest_arch_names_the_lowering_it_covers() {
        assert!(super::lowering(Isa::X86).ends_with("x86_64"));
        assert!(super::lowering(Isa::Aarch64).ends_with("aarch64"));
        assert_ne!(super::lowering(Isa::X86), super::lowering(Isa::Aarch64));
    }

    #[test]
    fn absent_prerequisites_name_the_command_that_builds_them() {
        let absent = std::path::Path::new("/absent");
        let missing = super::missing(absent, absent, absent, absent, "amd64");
        assert_eq!(missing.len(), 4);
        assert!(missing[0].contains("make bench-guest BENCH_ARCH=amd64"));
        assert!(missing[1].contains("cargo build --release --locked -p engine --bin hl-engine"));
    }

    #[test]
    fn wiring_never_selects_the_exec_wrapper_as_the_engine() {
        let (engine, runner) = wiring(std::path::Path::new("/build"), Isa::Aarch64);
        assert!(engine.ends_with("linux-production/hl-engine-linux-aarch64"));
        assert!(runner.ends_with("bin/hl-engine-runner"));
        assert_ne!(engine, runner);
    }

    #[test]
    fn baseline_round_trips_engine_pin_and_samples() {
        let mut samples = std::collections::BTreeMap::new();
        samples.insert("compute".to_owned(), Statistics::of(&[41, 42, 43]).unwrap());
        let key = |arch: &str| (arch.to_owned(), "compute".to_owned(), "compute".to_owned());
        let text = Baseline::default().render(&provenance(), "arm64", "compute", &samples);
        let parsed = Baseline::parse(&text).unwrap();
        assert_eq!(parsed.engines["arm64"], "abc");
        assert_eq!(parsed.revision.as_deref(), Some("0123456789ab"));
        assert_eq!(parsed.samples[&key("arm64")], 42);
        assert!(Baseline::parse("sample\tonly\ttwo\n").is_err());

        let kept = Baseline::parse(&parsed.render(&provenance(), "amd64", "compute", &samples)).unwrap();
        assert_eq!(kept.samples.len(), 2);
        assert_eq!(kept.engines.len(), 2);
        assert!(kept.samples.contains_key(&key("arm64")));
    }

    #[test]
    fn dirty_trees_are_never_recorded_and_revisions_are_reported() {
        assert!(!provenance().dirty());
        assert!(
            Provenance {
                revision: "0123456789ab-dirty".into(),
                ..provenance()
            }
            .dirty()
        );
        assert!(!revision().is_empty());
    }

    const HEADER: &str = "env,arch,phase,us,ok,guest_sha256,engine_sha256";

    fn row(path: &std::path::Path, us: u64, engine: &str) {
        std::fs::write(
            path,
            format!("{HEADER}\nrust-engine,arm64,compute,{us},7,g,{engine}\n"),
        )
        .unwrap();
    }

    /// Writes one cycle file per provider, each carrying `phases` as `(name, ok)`.
    fn cycle(directory: &std::path::Path, name: &str, arms: &[(&str, &[(&str, u64)])]) -> Vec<std::path::PathBuf> {
        arms.iter()
            .map(|(provider, phases)| {
                let path = directory.join(format!("{name}-{provider}.csv"));
                let mut text = format!("{HEADER}\n");
                for (phase, ok) in *phases {
                    text.push_str(&format!("{provider},arm64,{phase},10,{ok},g,e\n"));
                }
                std::fs::write(&path, text).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn collect_groups_samples_and_refuses_mixed_builds() {
        let directory = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for (index, us) in [10_u64, 12].into_iter().enumerate() {
            let path = directory.path().join(format!("cycle-{index}.csv"));
            row(&path, us, "same");
            paths.push(path);
        }
        let series = collect(&paths).unwrap();
        assert_eq!(series[&("compute".to_owned(), "rust-engine".to_owned())], [10, 12]);

        let foreign = directory.path().join("foreign.csv");
        row(&foreign, 11, "other");
        paths.push(foreign);
        assert!(collect(&paths).unwrap_err().contains("different trees"));
    }

    #[test]
    fn a_healthy_run_where_every_arm_did_the_same_work_still_yields_a_series() {
        let directory = tempfile::tempdir().unwrap();
        let full: &[(&str, u64)] = &[("compute", 40), ("file", 400), ("syscall", 11)];
        let mut paths = cycle(directory.path(), "c1", &[("c-engine", full), ("rust-engine", full)]);
        // The syscall checksum is thread-id derived, so a divergent one must not refuse.
        let second: &[(&str, u64)] = &[("compute", 40), ("file", 400), ("syscall", 99)];
        paths.extend(cycle(
            directory.path(),
            "c2",
            &[("c-engine", full), ("rust-engine", second)],
        ));
        let series = collect(&paths).unwrap();
        assert_eq!(series[&("compute".to_owned(), "rust-engine".to_owned())].len(), 2);
    }

    #[test]
    fn an_arm_that_truncated_a_phase_is_refused_by_name_with_both_checksums() {
        let directory = tempfile::tempdir().unwrap();
        let paths = cycle(
            directory.path(),
            "c1",
            &[
                ("c-engine", &[("compute", 40), ("file", 400)]),
                ("rust-engine", &[("compute", 40), ("file", 137)]),
            ],
        );
        let error = collect(&paths).unwrap_err();
        assert!(error.contains("phase file did unequal work"), "{error}");
        assert!(error.contains("c-engine ok=400") && error.contains("rust-engine ok=137"), "{error}");
        assert!(!error.contains("phase compute"), "{error}");
    }

    #[test]
    fn an_arm_that_never_reached_a_phase_is_refused_as_missing_not_as_disagreement() {
        let directory = tempfile::tempdir().unwrap();
        let paths = cycle(
            directory.path(),
            "c1",
            &[
                ("c-engine", &[("compute", 40), ("file", 400)]),
                ("rust-engine", &[("compute", 40)]),
            ],
        );
        let error = collect(&paths).unwrap_err();
        assert!(error.contains("phase file has no rows for rust-engine"), "{error}");
    }

    #[test]
    fn an_arm_with_fewer_cycles_is_refused_even_when_its_checksum_matches() {
        let directory = tempfile::tempdir().unwrap();
        let phases: &[(&str, u64)] = &[("compute", 40), ("syscall", 11)];
        let mut paths = cycle(directory.path(), "c1", &[("c-engine", phases), ("rust-engine", phases)]);
        paths.extend(cycle(directory.path(), "c2", &[("c-engine", phases)]));
        let error = collect(&paths).unwrap_err();
        assert!(error.contains("unequal sample counts"), "{error}");
        assert!(error.contains("c-engine n=2") && error.contains("rust-engine n=1"), "{error}");
    }

    #[test]
    fn a_run_that_skipped_the_native_arm_is_not_treated_as_a_missing_arm() {
        let directory = tempfile::tempdir().unwrap();
        let phases: &[(&str, u64)] = &[("compute", 40)];
        let paths = cycle(directory.path(), "c1", &[("c-engine", phases), ("rust-engine", phases)]);
        assert!(collect(&paths).is_ok());
    }
}
