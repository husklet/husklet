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

struct Columns {
    count: usize,
    wall: Option<usize>,
    execution: Option<usize>,
}

#[derive(Clone, Copy)]
struct Reference {
    time: u64,
    wall: u64,
}

struct Output {
    baseline: String,
    references: BTreeMap<(String, String), Reference>,
    checksums: BTreeMap<(String, String), u64>,
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
    pub(crate) fn new(baseline: impl Into<String>, paths: Vec<PathBuf>) -> Self {
        Self {
            baseline: baseline.into(),
            paths,
        }
    }

    fn rows(path: &Path) -> Result<Vec<Row>, String> {
        let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut lines = BufReader::new(file).lines();
        let header = lines
            .next()
            .ok_or_else(|| format!("empty CSV: {}", path.display()))?
            .map_err(|error| error.to_string())?;
        let columns = Columns::from_header(&header);
        let mut rows = Vec::new();
        for line in lines {
            let line = line.map_err(|error| error.to_string())?;
            rows.push(Row::parse(&line, &columns, path)?);
        }
        Ok(rows)
    }

    pub(crate) fn write(self) -> Result<(), String> {
        let mut all = Vec::new();
        for path in self.paths {
            all.extend(Self::rows(&path)?);
        }
        let mut output = Output::new(self.baseline, &all);
        output.header();
        for row in all {
            output.row(row)?;
        }
        Ok(())
    }
}

impl Columns {
    fn from_header(header: &str) -> Self {
        let columns = header.split(',').collect::<Vec<_>>();
        Self {
            count: columns.len(),
            wall: columns.iter().position(|column| *column == "wall_us"),
            execution: columns.iter().position(|column| *column == "execution"),
        }
    }

    fn wall(&self, fields: &[&str]) -> Result<u64, String> {
        let Some(index) = self.wall else {
            return Ok(0);
        };
        fields
            .get(index)
            .ok_or_else(|| "missing wall time".to_string())?
            .parse()
            .map_err(|_| "invalid wall time".to_string())
    }

    fn execution(&self, fields: &[&str]) -> String {
        self.execution
            .and_then(|index| fields.get(index))
            .map_or_else(|| "unspecified".into(), |value| (*value).into())
    }
}

impl Row {
    fn parse(line: &str, columns: &Columns, path: &Path) -> Result<Self, String> {
        let fields = line.split(',').collect::<Vec<_>>();
        if fields.len() != columns.count || fields.len() < 8 {
            return Err(format!("invalid CSV row in {}", path.display()));
        }
        Ok(Self {
            provider: fields[0].into(),
            arch: fields[1].into(),
            phase: fields[2].into(),
            time: fields[3].parse().map_err(|_| "invalid time".to_string())?,
            checksum: fields[4].parse().map_err(|_| "invalid checksum".to_string())?,
            wall: columns.wall(&fields)?,
            execution: columns.execution(&fields),
        })
    }
}

impl Reference {
    fn ratio(value: u64, reference: u64) -> String {
        if reference == 0 {
            "-".into()
        } else {
            format!("{:.3}", value as f64 / reference as f64)
        }
    }
}

impl Output {
    fn new(baseline: String, rows: &[Row]) -> Self {
        let references = rows
            .iter()
            .filter(|row| row.provider == baseline)
            .map(|row| {
                (
                    (row.arch.clone(), row.phase.clone()),
                    Reference {
                        time: row.time,
                        wall: row.wall,
                    },
                )
            })
            .collect();
        Self {
            baseline,
            references,
            checksums: BTreeMap::new(),
        }
    }

    fn header(&self) {
        println!(
            "provider,arch,phase,execution,us,wall_us,x_{},wall_x_{},checksum",
            self.baseline, self.baseline
        );
    }

    fn row(&mut self, row: Row) -> Result<(), String> {
        let reference = self.references.get(&(row.arch.clone(), row.phase.clone())).copied();
        let ratio = reference.map_or_else(|| "-".into(), |value| Reference::ratio(row.time, value.time));
        let wall_ratio = reference.map_or_else(|| "-".into(), |value| Reference::ratio(row.wall, value.wall));
        if row.phase != "syscall"
            && self
                .checksums
                .insert((row.arch.clone(), row.phase.clone()), row.checksum)
                .is_some_and(|value| value != row.checksum)
        {
            return Err(format!("checksum divergence: {}/{}", row.arch, row.phase));
        }
        println!(
            "{},{},{},{},{},{},{},{},{}",
            row.provider, row.arch, row.phase, row.execution, row.time, row.wall, ratio, wall_ratio, row.checksum
        );
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
