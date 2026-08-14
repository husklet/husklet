//! A recorded corpus mark, and the diff of a sweep against it.
//!
//! The count moving is not the finding; a *new* failure is. So a mark lists every non-pass
//! `(id, target)` with a disposition, and any row it omits is expected to pass.

use super::{WorkKey, ledger};
use crate::suite::{Error, Target};
use std::collections::BTreeMap;
use std::path::Path;

const COMMENT: char = '#';

pub(super) struct Mark {
    /// Header `# corpus mark<TAB>key<TAB>value` lines, in file order.
    provenance: Vec<(String, String)>,
    expected: BTreeMap<WorkKey, Expectation>,
}

struct Expectation {
    status: String,
    disposition: String,
}

/// One row that moved, in the direction that decides whether the sweep is a regression.
pub(super) struct Change {
    pub regression: bool,
    pub line: String,
}

/// Judges a published ledger against a recorded mark.
pub(super) fn compare(mark: &Path, results: &Path) -> Result<(), Error> {
    let mark = load(mark)?;
    println!("runtime: baseline {}", mark.describe());
    let changes = mark.diff(&observed(results)?);
    if changes.is_empty() {
        println!("runtime: no case moved against the baseline");
        return Ok(());
    }
    for change in changes.iter().filter(|change| !change.regression) {
        println!("runtime: {}", change.line);
    }
    let regressions = changes
        .iter()
        .filter(|change| change.regression)
        .map(|change| change.line.as_str())
        .collect::<Vec<_>>();
    if regressions.is_empty() {
        Ok(())
    } else {
        Err(regressions.join("\n").into())
    }
}

pub(super) fn load(path: &Path) -> Result<Mark, Error> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut provenance = Vec::new();
    let mut expected = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with(COMMENT) {
            let mut fields = line.splitn(3, '\t');
            if let (Some(_), Some(key), Some(value)) = (fields.next(), fields.next(), fields.next()) {
                provenance.push((key.to_owned(), value.to_owned()));
            }
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields[0] == "id" {
            continue;
        }
        let where_ = || format!("{}:{}", path.display(), index + 1);
        if fields.len() < 4 {
            return Err(format!("{}: a mark row needs id, target, status, disposition", where_()).into());
        }
        let key = WorkKey {
            id: fields[0].to_owned(),
            target: Target::named(fields[1]).ok_or_else(|| format!("{}: unknown target", where_()))?,
        };
        if !matches!(fields[2], ledger::PASS | ledger::FAIL | ledger::NOT_RUN) {
            return Err(format!("{}: unknown status {}", where_(), fields[2]).into());
        }
        let expectation = Expectation {
            status: fields[2].to_owned(),
            disposition: fields[3].to_owned(),
        };
        if expected.insert(key, expectation).is_some() {
            return Err(format!("{}: duplicate (id, target)", where_()).into());
        }
    }
    Ok(Mark { provenance, expected })
}

impl Mark {
    /// A row the mark does not list was passing when the mark was taken.
    fn status_of(&self, key: &WorkKey) -> &str {
        self.expected.get(key).map_or(ledger::PASS, |row| row.status.as_str())
    }

    fn disposition_of(&self, key: &WorkKey) -> &str {
        self.expected.get(key).map_or("", |row| row.disposition.as_str())
    }

    pub fn describe(&self) -> String {
        self.provenance
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Compares observed rows against the mark. Rows the sweep did not run are not judged, so a
    /// focused `--case` run diffs exactly what it covered.
    pub fn diff(&self, observed: &BTreeMap<WorkKey, String>) -> Vec<Change> {
        let mut changes = Vec::new();
        for (key, status) in observed {
            let was = self.status_of(key);
            if was == status {
                continue;
            }
            let (label, regression) = match (was, status.as_str()) {
                (ledger::PASS, ledger::FAIL) => ("REGRESSION", true),
                (ledger::FAIL, ledger::PASS) => ("FIXED", false),
                (ledger::NOT_RUN, _) => ("ACTIVATED", false),
                (_, ledger::NOT_RUN) => ("DEACTIVATED", false),
                _ => ("CHANGED", true),
            };
            let disposition = self.disposition_of(key);
            let context = if disposition.is_empty() {
                String::new()
            } else {
                format!(" (mark disposition: {disposition})")
            };
            changes.push(Change {
                regression,
                line: format!("{label} {} {}: {was} -> {status}{context}", key.id, key.target.name()),
            });
        }
        changes
    }
}

/// Reads back the ledger the sweep just published, so the diff sees resumed rows too.
pub(super) fn observed(path: &Path) -> Result<BTreeMap<WorkKey, String>, Error> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut rows = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 || fields[0] == "id" {
            continue;
        }
        let Some(target) = Target::named(fields[1]) else {
            continue;
        };
        if !matches!(fields[3], ledger::PASS | ledger::FAIL | ledger::NOT_RUN) {
            continue;
        }
        rows.insert(
            WorkKey {
                id: fields[0].to_owned(),
                target,
            },
            fields[3].to_owned(),
        );
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{Mark, WorkKey, load, observed};
    use crate::suite::Target;
    use std::collections::BTreeMap;

    fn key(id: &str) -> WorkKey {
        WorkKey {
            id: id.to_owned(),
            target: Target::Arm64,
        }
    }

    fn mark(body: &str) -> (tempfile::TempDir, Mark) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("baseline.tsv");
        std::fs::write(&path, body).unwrap();
        let loaded = load(&path).unwrap();
        (directory, loaded)
    }

    #[test]
    fn the_committed_mark_parses_and_states_its_commit() {
        let path = super::super::workspace().unwrap().join("tests/runtime/baseline.tsv");
        let loaded = load(&path).unwrap();
        assert!(loaded.describe().contains("commit="));
        assert_eq!(loaded.status_of(&key("runtime/memory/map-flags")), "fail");
        assert_eq!(loaded.status_of(&key("runtime/abi-ackermann")), "pass");
    }

    #[test]
    fn a_new_failure_is_a_regression_and_a_known_one_is_not() {
        let (_directory, loaded) = mark(
            "# corpus mark\tcommit\tabc\nid\ttarget\tstatus\tdisposition\tnote\n\
             runtime/known\tarm64\tfail\tpre-existing\t\n",
        );
        let observed = BTreeMap::from([
            (key("runtime/known"), "fail".to_owned()),
            (key("runtime/fresh"), "fail".to_owned()),
        ]);
        let changes = loaded.diff(&observed);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].regression);
        assert!(changes[0].line.contains("runtime/fresh"));
    }

    #[test]
    fn a_repaired_case_is_reported_without_failing_the_sweep() {
        let (_directory, loaded) = mark("id\ttarget\tstatus\tdisposition\tnote\nruntime/known\tarm64\tfail\treal\t\n");
        let changes = loaded.diff(&BTreeMap::from([(key("runtime/known"), "pass".to_owned())]));
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].regression);
        assert!(changes[0].line.contains("FIXED"));
    }

    #[test]
    fn a_published_ledger_reads_back_as_observed_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("results.tsv");
        std::fs::write(
            &path,
            "id\ttarget\tprofile\tstatus\telapsed_ms\thost_load\tdiagnostic\n\
             runtime/a\tarm64\trelease\tpass\t1\t1/8\t\n\
             runtime/b\tamd64\trelease\tfail\t2\t1/8\tboom\n",
        )
        .unwrap();
        let rows = observed(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[&key("runtime/a")], "pass");
    }
}
