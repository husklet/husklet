// The expectation helpers take the owned records they classify.
#![allow(clippy::needless_pass_by_value)]
#![forbid(unsafe_code)]

use clap::Parser;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(about = "Inventory the authority of YAML test expectations")]
struct Options {
    /// Repository root containing tests/runtime and tests/scenarios.
    #[arg(long, default_value = ".")]
    repository: PathBuf,
    /// Fail when an expectation has no declared authority.
    #[arg(long)]
    require_classified: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Authority {
    Oracle,
    Authored,
    Unclassified,
}

#[derive(Default)]
struct Inventory {
    manifests: usize,
    cases: usize,
    expectations: BTreeMap<Authority, usize>,
}

fn main() {
    if let Err(error) = execute(Options::parse()) {
        eprintln!("testing-expectations: {error}");
        std::process::exit(1);
    }
}

fn execute(options: Options) -> Result<(), Box<dyn Error>> {
    let inventory = inventory(&options.repository)?;
    let count = |authority| inventory.expectations.get(&authority).copied().unwrap_or(0);
    println!(
        "manifests={} cases={} expectations={} oracle={} authored={} unclassified={}",
        inventory.manifests,
        inventory.cases,
        inventory.expectations.values().sum::<usize>(),
        count(Authority::Oracle),
        count(Authority::Authored),
        count(Authority::Unclassified),
    );
    if options.require_classified && count(Authority::Unclassified) != 0 {
        return Err("unclassified expectations remain; declare oracle or authored authority".into());
    }
    Ok(())
}

fn inventory(repository: &Path) -> Result<Inventory, Box<dyn Error>> {
    let mut paths = Vec::new();
    for suite in ["runtime", "scenarios", "bench"] {
        collect_manifests(&repository.join("tests").join(suite), &mut paths)?;
    }
    paths.sort();

    let mut inventory = Inventory::default();
    for path in paths {
        let document: Value = serde_yaml::from_slice(&fs::read(&path)?)?;
        let root = document
            .as_mapping()
            .ok_or_else(|| format!("{}: manifest root is not a mapping", path.display()))?;
        let authority = authority(root, &path)?;
        let cases = root
            .get(Value::String("cases".into()))
            .and_then(Value::as_sequence)
            .map(Vec::as_slice)
            .unwrap_or_default();
        inventory.manifests += 1;
        inventory.cases += cases.len();
        let expectations = cases.iter().filter(|case| has_expectation(case)).count();
        *inventory.expectations.entry(authority).or_default() += expectations;
    }
    Ok(inventory)
}

fn has_expectation(case: &&Value) -> bool {
    case.as_mapping()
        .is_some_and(|case| case.contains_key(Value::String("expect".into())))
}

fn authority(root: &Mapping, path: &Path) -> Result<Authority, Box<dyn Error>> {
    if let Some(oracle) = root.get(Value::String("oracle".into())) {
        let provider = oracle
            .as_mapping()
            .and_then(|value| value.get(Value::String("provider".into())))
            .and_then(Value::as_str);
        return provider.map_or_else(
            || Err(format!("{}: oracle provider must be declared", path.display()).into()),
            |_| Ok(Authority::Oracle),
        );
    }
    let Some(expectation) = root.get(Value::String("expectation".into())) else {
        return Ok(Authority::Unclassified);
    };
    let declared = expectation.as_str().or_else(|| {
        expectation
            .as_mapping()?
            .get(Value::String("authority".into()))?
            .as_str()
    });
    match declared {
        Some("authored") => Ok(Authority::Authored),
        Some("oracle") => Ok(Authority::Oracle),
        Some(other) => Err(format!("{}: unknown expectation authority {other:?}", path.display()).into()),
        None => Err(format!("{}: expectation authority must be a string", path.display()).into()),
    }
}

fn collect_manifests(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_manifests(&path, paths)?;
        } else if path.file_name().is_some_and(|name| name == "test.yaml") {
            paths.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Authority, authority};
    use clap::Parser;
    use serde_yaml::Value;

    #[test]
    fn authority_is_explicit_and_read_only() {
        let oracle: Value = serde_yaml::from_str("oracle: { provider: qemu }").unwrap();
        let authored: Value = serde_yaml::from_str("expectation: { authority: authored }").unwrap();
        let absent: Value = serde_yaml::from_str("cases: []").unwrap();
        let path = std::path::Path::new("test.yaml");
        assert_eq!(
            authority(oracle.as_mapping().unwrap(), path).unwrap(),
            Authority::Oracle
        );
        assert_eq!(
            authority(authored.as_mapping().unwrap(), path).unwrap(),
            Authority::Authored
        );
        assert_eq!(
            authority(absent.as_mapping().unwrap(), path).unwrap(),
            Authority::Unclassified
        );
        assert!(super::Options::try_parse_from(["testing-expectations", "--update"]).is_err());
    }
}
