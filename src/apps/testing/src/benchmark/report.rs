use clap::Args;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct Row {
    provider: String,
    arch: String,
    phase: String,
    time: u64,
    checksum: u64,
    wall: u64,
    execution: String,
}

/// A validated report request with at least one result source.
#[derive(Args)]
pub(crate) struct Report {
    #[arg(long, default_value = "native")]
    baseline: String,
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

impl Report {
    fn rows(path: &Path) -> Result<Vec<Row>, String> {
        let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut lines = BufReader::new(file).lines();
        let header = lines
            .next()
            .ok_or_else(|| format!("empty CSV: {}", path.display()))?
            .map_err(|error| error.to_string())?;
        let columns = header.split(',').collect::<Vec<_>>();
        let wall = columns.iter().position(|column| *column == "wall_us");
        let execution = columns.iter().position(|column| *column == "execution");
        let mut rows = Vec::new();
        for line in lines {
            let line = line.map_err(|error| error.to_string())?;
            let fields = line.split(',').collect::<Vec<_>>();
            if fields.len() != columns.len() || fields.len() < 8 {
                return Err(format!("invalid CSV row in {}", path.display()));
            }
            rows.push(Row {
                provider: fields[0].into(),
                arch: fields[1].into(),
                phase: fields[2].into(),
                time: fields[3].parse().map_err(|_| "invalid time".to_string())?,
                checksum: fields[4].parse().map_err(|_| "invalid checksum".to_string())?,
                wall: wall.map_or(Ok(0), |index| {
                    fields
                        .get(index)
                        .ok_or_else(|| "missing wall time".to_string())?
                        .parse()
                        .map_err(|_| "invalid wall time".to_string())
                })?,
                execution: execution
                    .and_then(|index| fields.get(index))
                    .map_or_else(|| "unspecified".into(), |value| (*value).into()),
            });
        }
        Ok(rows)
    }

    pub(crate) fn write(self) -> Result<(), String> {
        let mut all = Vec::new();
        for path in self.paths {
            all.extend(Self::rows(&path)?);
        }
        let mut references = BTreeMap::new();
        for row in &all {
            if row.provider == self.baseline {
                references.insert((row.arch.clone(), row.phase.clone()), (row.time, row.wall));
            }
        }
        println!(
            "provider,arch,phase,execution,us,wall_us,x_{},wall_x_{},checksum",
            self.baseline, self.baseline
        );
        let mut checksums = BTreeMap::new();
        for row in all {
            let ratio = references
                .get(&(row.arch.clone(), row.phase.clone()))
                .filter(|value| value.0 != 0)
                .map_or_else(
                    || "-".into(),
                    |value| format!("{:.3}", row.time as f64 / value.0 as f64),
                );
            let wall_ratio = references
                .get(&(row.arch.clone(), row.phase.clone()))
                .filter(|value| value.1 != 0)
                .map_or_else(
                    || "-".into(),
                    |value| format!("{:.3}", row.wall as f64 / value.1 as f64),
                );
            if row.phase != "syscall"
                && checksums
                    .insert((row.arch.clone(), row.phase.clone()), row.checksum)
                    .is_some_and(|value| value != row.checksum)
            {
                return Err(format!("checksum divergence: {}/{}", row.arch, row.phase));
            }
            println!(
                "{},{},{},{},{},{},{},{},{}",
                row.provider, row.arch, row.phase, row.execution, row.time, row.wall, ratio, wall_ratio, row.checksum
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Report;
    use std::fs;

    #[test]
    fn optional_columns() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("results.csv");
        fs::write(
            &path,
            "provider,arch,phase,time_us,checksum,a,b,c,wall_us,execution\n\
             native,aarch64,compute,10,42,0,0,0,12,jit\n",
        )
        .expect("write fixture");

        let parsed = Report::rows(&path).expect("parse results");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].wall, 12);
        assert_eq!(parsed[0].execution, "jit");
    }

    #[test]
    fn checksum_divergence() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("results.csv");
        fs::write(
            &path,
            "provider,arch,phase,time_us,checksum,a,b,c\n\
             native,aarch64,compute,10,42,0,0,0\n\
             rust,aarch64,compute,11,43,0,0,0\n",
        )
        .expect("write fixture");

        let error = Report {
            baseline: "native".into(),
            paths: vec![path],
        }
        .write()
        .expect_err("checksum must diverge");
        assert_eq!(error, "checksum divergence: aarch64/compute");
    }
}
