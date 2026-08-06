//! Authoritative Rust-vs-retained-C verdict: harness-enforced wiring and CPU
//! pinning, median statistics with a noise floor, and a committed baseline gate.

use super::{Isa, LIMIT, matrix::Matrix, parse_duration, workload::Workload};
use clap::Args;
use std::collections::BTreeMap;
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
    /// CPU the harness pins every provider to; defaults to the highest allowed CPU.
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
}

/// A committed baseline: the builds it was measured against and its samples.
#[derive(Default)]
pub(crate) struct Baseline {
    engine: Option<String>,
    revision: Option<String>,
    samples: BTreeMap<(String, String), u64>,
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
        })
    }

    pub(crate) fn spread(self) -> f64 {
        if self.median == 0 {
            return f64::INFINITY;
        }
        ratio(self.maximum - self.minimum, self.median)
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
                ["engine", "c-engine", build_id] => baseline.engine = Some((*build_id).to_owned()),
                ["engine", "rust-engine", revision] => baseline.revision = Some((*revision).to_owned()),
                ["sample", workload, phase, us] => {
                    let value = us.parse().map_err(|_| format!("invalid baseline sample: {line}"))?;
                    baseline.samples.insert(((*workload).to_owned(), (*phase).to_owned()), value);
                }
                _ => return Err(format!("invalid baseline row: {line}")),
            }
        }
        Ok(baseline)
    }

    fn render(
        provenance: &Provenance,
        workload: &str,
        samples: &BTreeMap<String, Statistics>,
    ) -> String {
        let mut text = String::from("# husklet benchmark baseline v1: rust-engine median microseconds\n");
        text.push_str(&format!("engine\tc-engine\t{}\n", provenance.build_id));
        text.push_str(&format!("engine\trust-engine\t{}\n", provenance.revision));
        for (phase, statistics) in samples {
            text.push_str(&format!("sample\t{workload}\t{phase}\t{}\n", statistics.median));
        }
        text
    }
}

/// Everything a quoted number must be traceable to.
pub(crate) struct Provenance {
    build_id: String,
    revision: String,
    rust_sha256: String,
}

impl Provenance {
    fn dirty(&self) -> bool {
        self.revision.ends_with("-dirty")
    }

    fn print(&self, workload: &str, cpu: usize, repeats: usize) {
        println!(
            "provenance\tworkload={workload}\trust_revision={}\trust_sha256={}\tc_build_id={}\tcpu={cpu}\trepeats={repeats}",
            self.revision, self.rust_sha256, self.build_id
        );
    }
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
    let last = *allowed.last().ok_or("unreadable CPU affinity")?;
    let cpu = requested.unwrap_or(last);
    if !allowed.contains(&cpu) {
        return Err(format!("CPU {cpu} is outside the inherited affinity {affinity}"));
    }
    Ok((cpu, allowed.as_slice() == [cpu]))
}

/// Resolves the retained engine and its exec wrapper from a build root.
pub(crate) fn wiring(root: &Path, isa: Isa) -> (PathBuf, PathBuf) {
    (
        root.join("linux-production").join(format!("hl-engine-linux-{}", isa.name())),
        root.join("bin").join("hl-engine-runner"),
    )
}

impl Gate {
    pub(super) fn execute(self, process: &super::adapter::Process) -> Result<(), String> {
        if let Some(status) = self.repin()? {
            std::process::exit(status);
        }
        if self.repeats < 3 || self.repeats > LIMIT {
            return Err(format!("gate repeats must be between 3 and {LIMIT}"));
        }
        let (engine, runner) = wiring(&self.c_build, self.isa);
        if !engine.is_file() {
            return Err(format!("retained C engine is missing: {}", engine.display()));
        }
        if !runner.is_file() {
            return Err(format!("retained C exec wrapper is missing: {}", runner.display()));
        }
        let provenance = Provenance {
            build_id: super::matrix::elf_build_id(&engine)?,
            revision: revision(),
            rust_sha256: super::file_identity(&self.rust_engine)?,
        };
        let (cpu, _) = pinning(&super::host_affinity(), self.cpu)?;
        provenance.print(self.workload.name(), cpu, self.repeats);
        let baseline = self.baseline()?;
        if let Some(pinned) = baseline.engine.as_deref() {
            if !self.update && !pinned.eq_ignore_ascii_case(&provenance.build_id) {
                return Err(format!(
                    "retained C build changed: baseline pins {pinned}, {} has {}; re-record with --update",
                    engine.display(),
                    provenance.build_id
                ));
            }
        }
        if self.update && provenance.dirty() {
            return Err("refusing to record a baseline from a dirty tree; commit first".into());
        }
        let paths = self.measure(process, engine, runner, &provenance.build_id)?;
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
        println!("workload\tphase\tnative_us\tc_us\trust_us\trust_x_c\trust_x_native\tspread\tbaseline_x");
        let mut noisy = Vec::new();
        let mut regressed = Vec::new();
        let mut rust = BTreeMap::new();
        for (phase, providers) in &summaries {
            let pick = |name: &str| providers.get(name).copied();
            let (Some(native), Some(c), Some(engine)) = (pick("native"), pick("c-engine"), pick("rust-engine")) else {
                return Err(format!("phase {phase} is missing a provider row"));
            };
            let spread = [native, c, engine].map(Statistics::spread).into_iter().fold(0.0, f64::max);
            if spread > self.max_spread {
                noisy.push(format!("{phase} spread {spread:.3}"));
            }
            let against = baseline
                .samples
                .get(&(self.workload.name().to_owned(), phase.clone()))
                .map_or_else(|| "-".to_owned(), |value| format!("{:.3}", ratio(engine.median, *value)));
            println!(
                "{}\t{phase}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{spread:.3}\t{against}",
                self.workload.name(),
                native.median,
                c.median,
                engine.median,
                ratio(engine.median, c.median),
                ratio(engine.median, native.median),
            );
            if let Some(value) = baseline.samples.get(&(self.workload.name().to_owned(), phase.clone())) {
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
            std::fs::write(&self.baseline, Baseline::render(provenance, self.workload.name(), &rust))
                .map_err(|error| format!("write baseline: {error}"))?;
            println!("baseline\t{}\trecorded", self.baseline.display());
            return Ok(());
        }
        println!(
            "baseline\trevision={}\tcompared_against={}",
            baseline.revision.as_deref().unwrap_or("none"),
            provenance.revision
        );
        if regressed.is_empty() {
            println!("verdict\tpass\t{} phases within {:.3}", rust.len(), self.tolerance);
            Ok(())
        } else {
            Err(format!("rust-engine regressed: {}", regressed.join(", ")))
        }
    }
}

type Series = BTreeMap<(String, String), Vec<u64>>;

fn collect(paths: &[PathBuf]) -> Result<Series, String> {
    let mut series: Series = BTreeMap::new();
    let mut builds: BTreeMap<String, String> = BTreeMap::new();
    for path in paths {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut lines = text.lines();
        let header = lines.next().ok_or_else(|| format!("empty row file: {}", path.display()))?;
        let columns = header.split(',').collect::<Vec<_>>();
        let index = |name: &str| {
            columns
                .iter()
                .position(|column| *column == name)
                .ok_or_else(|| format!("{} lacks column {name}", path.display()))
        };
        let (provider, phase, us) = (index("env")?, index("phase")?, index("us")?);
        let (guest, engine) = (index("guest_sha256")?, index("engine_sha256")?);
        for line in lines {
            let fields = line.split(',').collect::<Vec<_>>();
            let field = |position: usize| fields.get(position).copied().ok_or_else(|| "short benchmark row".to_owned());
            let value = field(us)?.parse().map_err(|_| "invalid benchmark time".to_owned())?;
            let identity = format!("{}/{}", field(guest)?, field(engine)?);
            match builds.entry(field(provider)?.to_owned()).or_insert_with(|| identity.clone()) {
                recorded if *recorded == identity => {}
                recorded => {
                    return Err(format!(
                        "refusing to compare rows from different trees: {} measured {recorded} and {identity}",
                        field(provider)?
                    ));
                }
            }
            series
                .entry((field(phase)?.to_owned(), field(provider)?.to_owned()))
                .or_default()
                .push(value);
        }
    }
    Ok(series)
}

#[cfg(test)]
mod tests {
    use super::{Baseline, Isa, Provenance, Statistics, collect, pinning, revision, wiring};

    fn provenance() -> Provenance {
        Provenance {
            build_id: "abc".into(),
            revision: "0123456789ab".into(),
            rust_sha256: "feed".into(),
        }
    }

    #[test]
    fn statistics_report_median_and_spread() {
        let statistics = Statistics::of(&[100, 104, 102]).unwrap();
        assert_eq!(statistics.median, 102);
        assert!((statistics.spread() - 4.0 / 102.0).abs() < 1e-9);
        assert!(Statistics::of(&[]).is_none());
    }

    #[test]
    fn pinning_selects_one_cpu_and_rejects_foreign_requests() {
        assert_eq!(pinning("0-17", None).unwrap(), (17, false));
        assert_eq!(pinning("5", None).unwrap(), (5, true));
        assert_eq!(pinning("0-3,8", Some(8)).unwrap(), (8, false));
        assert!(pinning("0-3", Some(9)).is_err());
        assert!(pinning("unknown", None).is_err());
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
        samples.insert(
            "compute".to_owned(),
            Statistics {
                median: 42,
                minimum: 41,
                maximum: 43,
            },
        );
        let text = Baseline::render(&provenance(), "compute", &samples);
        let parsed = Baseline::parse(&text).unwrap();
        assert_eq!(parsed.engine.as_deref(), Some("abc"));
        assert_eq!(parsed.revision.as_deref(), Some("0123456789ab"));
        assert_eq!(parsed.samples[&("compute".to_owned(), "compute".to_owned())], 42);
        assert!(Baseline::parse("sample\tonly\ttwo\n").is_err());
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

    fn row(path: &std::path::Path, us: u64, engine: &str) {
        std::fs::write(
            path,
            format!("env,arch,phase,us,guest_sha256,engine_sha256\nrust-engine,arm64,compute,{us},g,{engine}\n"),
        )
        .unwrap();
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
}
