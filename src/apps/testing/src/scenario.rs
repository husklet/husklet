mod definition;
mod execution;
mod isolation;
mod ledger;
mod options;
mod process;
mod report;
mod scheduler;

use crate::suite::{self, Error};
use definition::Scenario;
pub(crate) use options::Options;
pub(crate) use report::{CachePreflightOptions, ProvenanceOptions};
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
    let result = load_scenarios(options.scenario.as_deref())?;
    let result = options.select_cases(result)?;
    if result.is_empty() {
        let root = workspace()?.join("tests/scenarios");
        return Err(format!("no scenarios matched under {}", root.display()).into());
    }
    Ok(result)
}

fn load_scenarios(selected: Option<&str>) -> Result<Vec<Scenario>, Error> {
    let root = workspace()?.join("tests/scenarios");
    let mut result = Vec::new();
    for manifest in suite::manifests(&root, selected)? {
        result.push(Scenario::load(&manifest.directory, &manifest.definition)?);
    }
    if result.is_empty() {
        return Err(format!("no scenarios matched under {}", root.display()).into());
    }
    Ok(result)
}

pub(crate) fn inventory() -> Result<(), Error> {
    report::inventory(load_scenarios(None)?)
}

pub(crate) fn provenance(options: ProvenanceOptions) -> Result<(), Error> {
    report::provenance(load_scenarios(None)?, options)
}

pub(crate) fn workflows() {
    report::workflows();
}

pub(crate) fn cache_preflight(options: CachePreflightOptions) -> Result<(), Error> {
    report::cache_preflight(load_scenarios(None)?, options)
}

fn workspace() -> Result<PathBuf, Error> {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !path.join("tests/scenarios").is_dir() {
        path = path.parent().ok_or("workspace root not found")?;
    }
    Ok(path.to_path_buf())
}
