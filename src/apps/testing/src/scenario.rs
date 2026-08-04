mod definition;
mod execution;

use definition::Scenario;
use std::path::{Path, PathBuf};

use crate::suite::{Error, Target};
use clap::Args;

pub async fn run(options: Options) -> Result<(), Error> {
    let mut passed = 0_usize;
    let mut expected_failures = 0_usize;
    let mut failed = Vec::new();

    for scenario in scenarios(&options)? {
        for target in options.targets() {
            for result in execution::run(&scenario, target).await? {
                match result {
                    execution::CaseResult::Passed(id) => {
                        println!("PASS {id} {}", target.name());
                        passed += 1;
                    }
                    execution::CaseResult::Failed(id, error) => {
                        println!("FAIL {id} {}: {error}", target.name());
                        failed.push(format!("{id} {}: {error}", target.name()));
                    }
                    execution::CaseResult::ExpectedFailure(id, error) => {
                        println!("XFAIL {id} {}: {error}", target.name());
                        expected_failures += 1;
                    }
                    execution::CaseResult::UnexpectedPass(id) => {
                        println!("XPASS {id} {}", target.name());
                        failed.push(format!("{id} {}: unexpected pass", target.name()));
                    }
                }
            }
        }
    }

    println!(
        "scenarios: {passed} passed; {expected_failures} expected failures; {} failed",
        failed.len()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n").into())
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
}

impl Options {
    fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }
}
