mod definition;
mod execution;
mod isolation;
mod ledger;
mod options;
mod process;
mod scheduler;

use crate::suite::{self, Error};
use definition::Scenario;
pub(crate) use options::Options;
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
    let mut result = Vec::new();
    for manifest in suite::manifests(&root, options.scenario.as_deref())? {
        result.push(Scenario::load(&manifest.directory, &manifest.definition)?);
    }
    let result = options.select_cases(result)?;
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
