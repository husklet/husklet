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
    #[arg(long, default_value_t = 6)]
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
mod provenance;
#[cfg(test)]
use provenance::{ENGINE_BUILD, artifact, slot};
pub(crate) use provenance::{Provenance, missing, pinning, revision, wiring};
use provenance::{build_engine, ratio};
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
            self.isa.lowering(),
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
        let status = crate::platform::HostProcess::standard("taskset")
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
            record_regression(&mut regressed, phase, engine, recorded, self.tolerance, spread);
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

fn record_regression(
    regressed: &mut Vec<String>,
    phase: &str,
    engine: Statistics,
    recorded: Option<&u64>,
    tolerance: f64,
    spread: f64,
) {
    let Some(value) = recorded else { return };
    let allowance = 1.0 + tolerance.max(spread);
    if ratio(engine.median, *value) > allowance {
        regressed.push(format!("{phase} {}us over baseline {value}us", engine.median));
    }
}

type Series = BTreeMap<(String, String), Vec<u64>>;

/// Work done per `(phase, provider)`: the summed `ok=` checksum and the rows it came from.
struct Totals(BTreeMap<(String, String), (u64, usize)>);

/// Pins each provider to the one `guest/engine` pair its first row was measured with.
///
/// A ratio between rows from two different trees is not a measurement of anything, so the second
/// identity a provider presents is refused rather than averaged in.
fn record_build_identity(builds: &mut BTreeMap<String, String>, provider: &str, identity: &str) -> Result<(), String> {
    let recorded = builds.entry(provider.to_owned()).or_insert_with(|| identity.to_owned());
    if recorded == identity {
        return Ok(());
    }
    Err(format!(
        "refusing to compare rows from different trees: {provider} measured {recorded} and {identity}"
    ))
}

fn collect(paths: &[PathBuf]) -> Result<Series, String> {
    let mut series: Series = BTreeMap::new();
    let mut totals = Totals(BTreeMap::new());
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
            record_build_identity(&mut builds, field(provider)?, &identity)?;
            let checksum: u64 = field(ok)?
                .parse()
                .map_err(|_| "invalid benchmark checksum".to_owned())?;
            let arm = field(provider)?;
            super::timebase_verdict(field(phase)?, value, checksum).map_err(|error| format!("{arm}: {error}"))?;
            let key = (field(phase)?.to_owned(), field(provider)?.to_owned());
            let done = totals.0.entry(key.clone()).or_insert((0, 0));
            *done = (done.0.saturating_add(checksum), done.1 + 1);
            series.entry(key).or_default().push(value);
        }
    }
    totals.validate_identical_work()?;
    Ok(series)
}

/// Refuses a verdict unless every arm ran every phase to the same completion, so a
/// run that aborted partway cannot be read as a smaller-is-faster win.
impl Totals {
    fn validate_identical_work(&self) -> Result<(), String> {
        let providers = self.0.keys().map(|(_, provider)| provider).collect::<BTreeSet<_>>();
        let mut phases: BTreeMap<&String, BTreeMap<&String, (u64, usize)>> = BTreeMap::new();
        for ((phase, provider), done) in &self.0 {
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
            if measured.values().map(|(ok, _)| *ok).collect::<BTreeSet<_>>().len() > 1 {
                return Err(format!(
                    "refusing the verdict: phase {phase} did unequal work across arms ({}); an arm stopped early and its smaller ok= is not a speedup",
                    describe(&measured, |(ok, rows)| format!("ok={ok} over {rows} samples"))
                ));
            }
        }
        Ok(())
    }
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
mod test;
