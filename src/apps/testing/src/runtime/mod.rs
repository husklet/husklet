pub(crate) mod definition;
mod execution;
pub(crate) mod image;

use definition::{App, Target};
use std::path::{Path, PathBuf};

type Error = Box<dyn std::error::Error>;

pub async fn run(arguments: &[String]) -> Result<(), Error> {
    let options = Options::parse(arguments)?;
    let apps = apps(&options)?;
    let mut failed = Vec::new();
    let mut passed = 0_usize;
    for app in apps {
        for target in options.targets() {
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
    println!("runtime: {passed} passed; {} failed", failed.len());
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

pub fn oracle(arguments: &[String]) -> Result<(), Error> {
    let (update, filtered) = match arguments.first().map(String::as_str) {
        Some("--update") => (true, &arguments[1..]),
        Some("--check") => (false, &arguments[1..]),
        _ => (false, arguments),
    };
    let options = Options::parse(filtered)?;
    for app in apps(&options)? {
        for target in options.targets() {
            app.oracle(target, update)?;
        }
    }
    Ok(())
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

struct Options {
    app: Option<String>,
    target: Option<Target>,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, Error> {
        let mut app = None;
        let mut target = None;
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--isa" => {
                    index += 1;
                    target = Some(Target::parse(arguments.get(index).ok_or("--isa requires a value")?)?);
                }
                value if value.starts_with('-') => return Err(format!("unknown option {value:?}").into()),
                value if app.is_none() => app = Some(value.to_owned()),
                value => return Err(format!("unexpected argument {value:?}").into()),
            }
            index += 1;
        }
        Ok(Self { app, target })
    }

    fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }
}
