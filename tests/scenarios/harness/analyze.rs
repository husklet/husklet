//! Read-only analysis of an active scenario batch.

use crate::report::{Attempt, ScenarioKey, ScenarioOutcome};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
pub struct Partial {
    pub run_id: String,
    pub archive_hashes: Vec<String>,
    pub attempts: usize,
    pub completed: usize,
    pub status: BTreeMap<String, usize>,
    pub category: BTreeMap<String, usize>,
    pub image: BTreeMap<String, usize>,
    pub failures: BTreeMap<String, usize>,
    pub comparison: Option<Comparison>,
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub run_id: String,
    pub archive_hashes: Vec<String>,
    pub completed_delta: i64,
    pub status_delta: BTreeMap<String, i64>,
}

pub fn generate(run: &Path, compare: Option<&Path>) -> io::Result<Partial> {
    let attempts: Vec<Attempt> = valid_lines(&run.join("attempts.jsonl"))?;
    let outcomes = latest::<ScenarioOutcome, _>(&run.join("results.jsonl"), |value| &value.key)?;
    let baseline = compare.map(load_counts).transpose()?;
    let mut value = counts(run, attempts.len(), outcomes.into_values());
    value.comparison = baseline.map(|other| Comparison {
        run_id: other.run_id,
        archive_hashes: other.archive_hashes,
        completed_delta: signed_delta(value.completed, other.completed),
        status_delta: delta(&value.status, &other.status),
    });
    atomic(
        &run.join("partial-summary.json"),
        &serde_json::to_vec_pretty(&value).map_err(io::Error::other)?,
    )?;
    atomic(&run.join("partial-summary.md"), markdown(&value).as_bytes())?;
    Ok(value)
}

fn load_counts(run: &Path) -> io::Result<Partial> {
    let attempts: Vec<Attempt> = valid_lines(&run.join("attempts.jsonl"))?;
    let outcomes = latest::<ScenarioOutcome, _>(&run.join("results.jsonl"), |value| &value.key)?;
    Ok(counts(run, attempts.len(), outcomes.into_values()))
}

fn counts(run: &Path, attempts: usize, outcomes: impl Iterator<Item = ScenarioOutcome>) -> Partial {
    let mut value = Partial {
        run_id: run
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        archive_hashes: Vec::new(),
        attempts,
        completed: 0,
        status: BTreeMap::new(),
        category: BTreeMap::new(),
        image: BTreeMap::new(),
        failures: BTreeMap::new(),
        comparison: None,
    };
    for outcome in outcomes {
        value.completed += 1;
        bump(
            &mut value.status,
            format!("{:?}", outcome.status).to_lowercase(),
        );
        bump(&mut value.category, outcome.category);
        bump(&mut value.image, outcome.declared_image);
        if !value
            .archive_hashes
            .contains(&outcome.key.engine_archive_hash)
        {
            value.archive_hashes.push(outcome.key.engine_archive_hash);
        }
        if outcome.status != crate::report::Status::InfrastructureFail {
            if let Some(error) = outcome.error {
                bump(&mut value.failures, normalize(&error));
            }
        }
    }
    value.archive_hashes.sort();
    value
}

fn valid_lines<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    let bytes = match fs::read(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect())
}

fn latest<T, F>(path: &Path, key: F) -> io::Result<BTreeMap<ScenarioKey, T>>
where
    T: serde::de::DeserializeOwned,
    F: Fn(&T) -> &ScenarioKey,
{
    let mut values = BTreeMap::new();
    for value in valid_lines(path)? {
        values.insert(key(&value).clone(), value);
    }
    Ok(values)
}

fn normalize(error: &str) -> String {
    let salient = [
        "psql: error:",
        "timed out",
        "connection refused",
        "stat: cannot statx",
        "not found",
    ]
    .into_iter()
    .find_map(|marker| error.find(marker).map(|offset| &error[offset..]))
    .unwrap_or(error);
    let salient = salient.lines().next().unwrap_or(salient);
    salient
        .split_whitespace()
        .map(|token| {
            if token.starts_with('/') {
                "<path>".into()
            } else {
                let mut output = String::new();
                let mut digits = false;
                for character in token.chars() {
                    if character.is_ascii_digit() {
                        if !digits {
                            output.push('#');
                        }
                        digits = true;
                    } else {
                        digits = false;
                        output.push(character);
                    }
                }
                output
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn bump(values: &mut BTreeMap<String, usize>, key: String) {
    *values.entry(key).or_default() += 1;
}
fn delta(now: &BTreeMap<String, usize>, old: &BTreeMap<String, usize>) -> BTreeMap<String, i64> {
    let mut value = BTreeMap::new();
    for key in now.keys().chain(old.keys()) {
        value.insert(
            key.clone(),
            signed_delta(*now.get(key).unwrap_or(&0), *old.get(key).unwrap_or(&0)),
        );
    }
    value
}
fn signed_delta(now: usize, old: usize) -> i64 {
    i64::try_from(now).unwrap_or(i64::MAX) - i64::try_from(old).unwrap_or(i64::MAX)
}
fn markdown(value: &Partial) -> String {
    let mut text = format!(
        "# Partial `{}`\n\nAttempts: {}; completed: {}\n\n## Status\n",
        value.run_id, value.attempts, value.completed
    );
    for (key, count) in &value.status {
        writeln!(text, "- {key}: {count}").expect("writing to a String cannot fail");
    }
    text.push_str("\n## Failure clusters\n");
    let mut failures = value.failures.iter().collect::<Vec<_>>();
    failures.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (key, count) in failures {
        writeln!(text, "- {count} × `{key}`").expect("writing to a String cannot fail");
    }
    text
}
fn atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

pub(crate) mod tests {
    pub(crate) fn normalization_removes_volatile_values() {
        assert_eq!(
            super::normalize("pid 123 at /tmp/a-99 time=2026-07-16T12:34:56"),
            "pid # at <path> time=#-#-#T#:#:#"
        );
        assert_eq!(
            super::normalize("checks=15 output=psql: error: socket /tmp/a pid 12\nlog"),
            super::normalize("checks=16 output=psql: error: socket /tmp/b pid 99\nlog")
        );
    }
}
