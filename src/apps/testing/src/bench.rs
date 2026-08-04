mod definition;
mod execution;

use crate::runtime::{self, definition::Target};
use definition::Benchmark;
use std::path::PathBuf;

type Error = Box<dyn std::error::Error>;

pub async fn run(arguments: &[String]) -> Result<(), Error> {
    let options = Options::parse(arguments)?;
    let mut passed = 0_usize;
    let mut failures = Vec::new();
    for benchmark in benchmarks(options.name.as_deref())? {
        for target in options.targets() {
            for result in execution::run(&benchmark, target).await? {
                match result {
                    execution::Result::Passed { id, samples } => {
                        let total = samples.iter().sum::<u128>();
                        let average = total / samples.len() as u128;
                        println!(
                            "PASS bench/{id} {} samples_ms={samples:?} average_ms={average}",
                            target.name()
                        );
                        passed += 1;
                    }
                    execution::Result::Failed { id, reason } => {
                        println!("FAIL bench/{id} {}: {reason}", target.name());
                        failures.push(format!("bench/{id} {}: {reason}", target.name()));
                    }
                }
            }
        }
    }
    println!("bench: {passed} passed; {} failed", failures.len());
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

fn benchmarks(selected: Option<&str>) -> Result<Vec<Benchmark>, Error> {
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
        if selected.is_some_and(|wanted| wanted != name) {
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

struct Options {
    name: Option<String>,
    target: Option<Target>,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, Error> {
        let (mut name, mut target, mut index) = (None, None, 0);
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--isa" => {
                    index += 1;
                    target = Some(Target::parse(arguments.get(index).ok_or("--isa requires a value")?)?);
                }
                value if value.starts_with('-') => return Err(format!("unknown option {value:?}").into()),
                value if name.is_none() => name = Some(value.to_owned()),
                value => return Err(format!("unexpected argument {value:?}").into()),
            }
            index += 1;
        }
        Ok(Self { name, target })
    }

    fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }
}
