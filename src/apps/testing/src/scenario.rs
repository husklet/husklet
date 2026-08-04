mod definition;
mod execution;
mod ledger;
mod scheduler;

use crate::suite::{Error, Target};
use clap::Args;
use definition::{Class, Scenario};
use std::path::{Path, PathBuf};

pub async fn run(options: Options) -> Result<(), Error> {
    let scenarios = scenarios(&options)?;
    if options.list {
        let work = scheduler::inventory(scenarios, &options)?;
        for key in &work {
            println!("{}\t{}", key.id, key.target.name());
        }
        println!("scenarios: {} selected case/target pairs", work.len());
        return Ok(());
    }
    let report = workspace()?.join(&options.results);
    let summary = scheduler::run(scenarios, &options, &report).await?;
    println!(
        "scenarios: {} passed; {} expected failures; {} failed",
        summary.passed,
        summary.expected_failures,
        summary.failed.len()
    );
    if summary.failed.is_empty() {
        Ok(())
    } else {
        Err(summary.failed.join("\n").into())
    }
}

fn scenarios(options: &Options) -> Result<Vec<Scenario>, Error> {
    let root = workspace()?.join("tests/scenarios");
    let mut directories = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    directories.sort();
    let mut result = Vec::new();
    for directory in directories.into_iter().filter(|value| value.is_dir()) {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if options.scenario.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let definition = directory.join("test.yaml");
        if definition.is_file() {
            result.push(Scenario::load(&directory, &definition)?);
        }
    }
    if result.is_empty() {
        return Err(format!("no scenarios matched under {}", root.display()).into());
    }
    Ok(result)
}

fn workspace() -> Result<PathBuf, Error> {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !path.join("tests/scenarios").is_dir() {
        path = path.parent().ok_or("workspace root not found")?;
    }
    Ok(path.to_path_buf())
}

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named scenario.
    scenario: Option<String>,
    /// Run only one guest ISA.
    #[arg(long = "isa", value_enum)]
    target: Option<Target>,
    /// Run only quick or long cases.
    #[arg(long, value_enum)]
    class: Option<Class>,
    /// Print selected case/target pairs without materializing images.
    #[arg(long)]
    list: bool,
    /// Maximum number of concurrently executing cases.
    #[arg(long, env = "HL_COMPAT_JOBS", default_value_t = logical_jobs(), value_parser = parse_jobs)]
    jobs: usize,
    /// Resume completed case/target keys from the durable partial result.
    #[arg(long, env = "HL_COMPAT_RESUME", default_value_t = false)]
    resume: bool,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/scenarios/results.tsv", value_parser = parse_results)]
    results: PathBuf,
}

fn logical_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(256)
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "jobs must be an integer".to_owned())?;
    (1..=256)
        .contains(&jobs)
        .then_some(jobs)
        .ok_or_else(|| "jobs must be between 1 and 256".to_owned())
}

fn parse_results(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        Err("results must be a safe relative path".to_owned())
    } else {
        Ok(path)
    }
}

impl Options {
    fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, logical_jobs, parse_jobs};
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        options: Options,
    }

    #[test]
    fn jobs_are_positive_and_bounded() {
        assert_eq!(parse_jobs("1"), Ok(1));
        assert_eq!(parse_jobs("256"), Ok(256));
        for invalid in ["0", "257", "many"] {
            assert!(parse_jobs(invalid).is_err(), "accepted {invalid}");
        }
        assert!((1..=256).contains(&logical_jobs()));
    }

    #[test]
    fn inventory_and_class_selection_are_typed() {
        let cli = TestCli::try_parse_from(["scenarios", "languages", "--class", "quick", "--list"]).unwrap();
        assert!(cli.options.list);
        assert_eq!(cli.options.class, Some(super::Class::Quick));
        assert_eq!(cli.options.scenario.as_deref(), Some("languages"));
        assert!(TestCli::try_parse_from(["scenarios", "--class", "smoke"]).is_err());
    }
}
