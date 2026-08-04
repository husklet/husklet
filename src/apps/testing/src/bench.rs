mod definition;
mod execution;

use crate::{
    runtime,
    suite::{Error, Target},
};
use clap::Args;
use definition::Benchmark;
use std::path::PathBuf;

pub async fn run(options: Options) -> Result<(), Error> {
    let targets = options.targets();
    let mut report = Report::default();
    for benchmark in options.benchmarks()? {
        benchmark.execute(&targets, &mut report).await?;
    }
    report.finish()
}

struct Statistics {
    minimum: u128,
    median: u128,
    p90: u128,
    p99: u128,
    maximum: u128,
}

impl Statistics {
    fn from_samples(samples: &mut [u128]) -> Self {
        samples.sort_unstable();
        Self {
            minimum: samples[0],
            median: Self::percentile(samples, 50),
            p90: Self::percentile(samples, 90),
            p99: Self::percentile(samples, 99),
            maximum: samples[samples.len() - 1],
        }
    }

    fn percentile(samples: &[u128], percent: usize) -> u128 {
        let rank = percent.saturating_mul(samples.len()).saturating_add(99) / 100;
        samples[rank.saturating_sub(1).min(samples.len() - 1)]
    }
}

#[derive(Default)]
struct Report {
    passed: usize,
    failures: Vec<String>,
}

impl Report {
    fn passed(&mut self) {
        self.passed += 1;
    }

    fn failed(&mut self, failure: String) {
        self.failures.push(failure);
    }

    fn finish(self) -> Result<(), Error> {
        println!("bench: {} passed; {} failed", self.passed, self.failures.len());
        if self.failures.is_empty() {
            Ok(())
        } else {
            Err(self.failures.join("\n").into())
        }
    }
}

impl Benchmark {
    async fn execute(&self, targets: &[Target], report: &mut Report) -> Result<(), Error> {
        for &target in targets {
            for result in execution::run(self, target).await? {
                result.report(report);
            }
        }
        Ok(())
    }
}

impl execution::Result {
    fn report(self, report: &mut Report) {
        match self {
            Self::Passed(passed) => passed.report(report),
            Self::Failed(failed) => failed.report(report),
        }
    }
}

impl execution::Passed {
    fn report(mut self, report: &mut Report) {
        let statistics = Statistics::from_samples(&mut self.samples);
        println!(
            "PASS bench/{} {} cold_ms={} min_ms={} median_ms={} p90_ms={} p99_ms={} max_ms={} samples={} warmups={} image={:?} execution={:?}",
            self.id,
            self.provenance.target,
            self.cold,
            statistics.minimum,
            statistics.median,
            statistics.p90,
            statistics.p99,
            statistics.maximum,
            self.samples.len(),
            self.provenance.warmups,
            self.provenance.image,
            self.provenance.execution,
        );
        for (phase, mut times) in self.phases {
            let statistics = Statistics::from_samples(&mut times);
            println!(
                "PHASE bench/{}/{phase} {} min_us={} median_us={} p90_us={} p99_us={} max_us={} samples={}",
                self.id,
                self.provenance.target,
                statistics.minimum,
                statistics.median,
                statistics.p90,
                statistics.p99,
                statistics.maximum,
                times.len(),
            );
        }
        report.passed();
    }
}

impl execution::Failed {
    fn report(self, report: &mut Report) {
        println!("FAIL bench/{} {}: {}", self.id, self.target, self.reason);
        report.failed(format!("bench/{} {}: {}", self.id, self.target, self.reason));
    }
}

#[cfg(test)]
mod tests {
    use super::Statistics;

    #[test]
    fn nearest_rank_percentiles_are_stable_for_small_samples() {
        let mut samples = [100, 1, 20, 3, 4];
        let summary = Statistics::from_samples(&mut samples);
        assert_eq!(summary.minimum, 1);
        assert_eq!(summary.median, 4);
        assert_eq!(summary.p90, 100);
        assert_eq!(summary.p99, 100);
        assert_eq!(summary.maximum, 100);
        assert_eq!(Statistics::percentile(&samples, 50), 4);
    }
}

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named benchmark definition.
    name: Option<String>,
    /// Run only one guest ISA.
    #[arg(long = "isa", value_enum)]
    target: Option<Target>,
}

impl Options {
    fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }

    fn benchmarks(&self) -> Result<Vec<Benchmark>, Error> {
        let root = runtime::workspace()?.join("tests/bench");
        let mut directories = std::fs::read_dir(&root)?
            .map(|entry| entry.map(|value| value.path()))
            .collect::<Result<Vec<PathBuf>, _>>()?;
        directories.sort();
        let mut benchmarks = Vec::new();
        for directory in directories.into_iter().filter(|path| path.is_dir()) {
            let name = directory
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !self.selects(name) {
                continue;
            }
            let definition = directory.join("test.yaml");
            if definition.is_file() {
                benchmarks.push(Benchmark::load(&directory, &definition)?);
            }
        }
        if benchmarks.is_empty() {
            return Err(format!("no benchmarks matched under {}", root.display()).into());
        }
        Ok(benchmarks)
    }

    fn selects(&self, name: &str) -> bool {
        self.name.as_deref().is_none_or(|wanted| wanted == name)
    }
}
