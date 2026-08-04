pub(crate) mod definition;
mod execution;
pub(crate) mod image;
pub(crate) mod scheduler;

use crate::suite::{Error, Target};
use clap::Args;
use definition::App;
use std::path::{Path, PathBuf};

pub async fn run(options: Options) -> Result<(), Error> {
    let apps = apps(&options)?;
    let mut failed = Vec::new();
    let mut passed = 0_usize;
    let mut eligible = false;
    for app in apps {
        for target in options.targets() {
            if !app.supports(target) {
                continue;
            }
            if app.cases_for(target).next().is_none() {
                continue;
            }
            eligible = true;
            for result in execution::run(&app, target).await? {
                match result {
                    execution::CaseResult::Passed(id) => {
                        println!("PASS {id} {}", target.name());
                        passed += 1;
                    }
                    execution::CaseResult::Failed(id, error) => {
                        println!("FAIL {id} {}: {error}", target.name());
                        failed.push(format!("{id} {}: {error}", target.name()));
                    }
                }
            }
        }
    }
    if !eligible {
        return Err("no runtime cases support the selected target(s)".into());
    }
    println!("runtime: {passed} passed; {} failed", failed.len());
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

pub fn oracle(options: OracleOptions) -> Result<(), Error> {
    let _check_requested = options.check;
    let mut eligible = false;
    for app in apps(&options.selection)? {
        for target in options.selection.targets() {
            if !app.supports(target) {
                continue;
            }
            if app.cases_for(target).next().is_none() {
                continue;
            }
            eligible = true;
            app.oracle(target, options.update)?;
        }
    }
    if eligible {
        Ok(())
    } else {
        Err("no oracle cases support the selected target(s)".into())
    }
}

fn apps(options: &Options) -> Result<Vec<App>, Error> {
    let root = workspace()?.join("tests/runtime");
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
        if options.app.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let definition = directory.join("test.yaml");
        if definition.is_file() {
            result.push(App::load(&directory, &definition)?);
        }
    }
    if result.is_empty() {
        return Err(format!("no runtime apps matched under {}", root.display()).into());
    }
    Ok(result)
}

pub(super) fn workspace() -> Result<PathBuf, Error> {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !path.join("tests/runtime").is_dir() {
        path = path.parent().ok_or("workspace root not found")?;
    }
    Ok(path.to_path_buf())
}

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named runtime application.
    app: Option<String>,
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

#[derive(Args)]
pub(crate) struct OracleOptions {
    /// Replace checked golden output with oracle output.
    #[arg(long, conflicts_with = "check")]
    update: bool,
    /// Check oracle output against the golden (the default).
    #[arg(long)]
    check: bool,
    #[command(flatten)]
    selection: Options,
}
