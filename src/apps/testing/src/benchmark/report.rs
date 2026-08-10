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
    diagnostics: String,
    context: String,
}

struct Columns {
    count: usize,
    wall: Option<usize>,
    execution: Option<usize>,
    diagnostics: Option<usize>,
    context: Option<usize>,
}

#[derive(Clone, Copy)]
struct Reference {
    time: u64,
    wall: u64,
}

struct Output {
    baseline: String,
    references: BTreeMap<(String, String, String), Reference>,
    checksums: BTreeMap<(String, String, String), u64>,
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
        let all = Self::summaries(all)?;
        let mut output = Output::new(self.baseline, &all);
        output.header();
        for row in all {
            output.row(row)?;
        }
        Ok(())
    }

    fn summaries(rows: Vec<Row>) -> Result<Vec<Row>, String> {
        type Phase = (String, String, String);
        type Group = (String, String, String, String);

        let mut checksums = BTreeMap::<Phase, u64>::new();
        let mut counts = BTreeMap::<Phase, BTreeMap<String, usize>>::new();
        let mut groups = BTreeMap::<Group, (Row, Vec<u64>, Vec<u64>)>::new();
        for row in rows {
            let phase = (row.arch.clone(), row.phase.clone(), row.context.clone());
            if checksums
                .insert(phase.clone(), row.checksum)
                .is_some_and(|value| value != row.checksum)
            {
                return Err(format!("checksum divergence: {}/{}", row.arch, row.phase));
            }
            *counts
                .entry(phase)
                .or_default()
                .entry(row.provider.clone())
                .or_default() += 1;

            let key = (
                row.provider.clone(),
                row.arch.clone(),
                row.phase.clone(),
                row.context.clone(),
            );
            if let Some((first, times, walls)) = groups.get_mut(&key) {
                if first.execution != row.execution || first.diagnostics != row.diagnostics {
                    return Err(format!(
                        "inconsistent sample metadata: {}/{}/{}",
                        row.provider, row.arch, row.phase
                    ));
                }
                times.push(row.time);
                walls.push(row.wall);
            } else {
                let time = row.time;
                let wall = row.wall;
                groups.insert(key, (row, vec![time], vec![wall]));
            }
        }

        for ((arch, phase, _), providers) in &counts {
            let mut samples = providers.values();
            let Some(expected) = samples.next() else {
                continue;
            };
            if samples.any(|count| count != expected) {
                return Err(format!("unbalanced samples: {arch}/{phase}"));
            }
        }

        Ok(groups
            .into_values()
            .map(|(mut row, mut times, mut walls)| {
                row.time = Self::median(&mut times);
                row.wall = Self::median(&mut walls);
                row
            })
            .collect())
    }

    fn median(values: &mut [u64]) -> u64 {
        values.sort_unstable();
        values[values.len() / 2]
    }
}

impl Columns {
    fn from_header(header: &str) -> Self {
        let columns = header.split(',').collect::<Vec<_>>();
        Self {
            count: columns.len(),
            wall: columns.iter().position(|column| *column == "wall_us"),
            execution: columns.iter().position(|column| *column == "execution"),
            diagnostics: columns.iter().position(|column| *column == "diagnostics"),
            context: columns.iter().position(|column| *column == "phase_context"),
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

    fn named(index: Option<usize>, fields: &[&str]) -> String {
        index
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
            execution: Columns::named(columns.execution, &fields),
            diagnostics: Columns::named(columns.diagnostics, &fields),
            context: Columns::named(columns.context, &fields),
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
                    (row.arch.clone(), row.phase.clone(), row.context.clone()),
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
            "provider,arch,phase,execution,diagnostics,phase_context,us,wall_us,x_{},wall_x_{},checksum",
            self.baseline, self.baseline
        );
    }

    fn row(&mut self, row: Row) -> Result<(), String> {
        let phase = (row.arch.clone(), row.phase.clone(), row.context.clone());
        let reference = self.references.get(&phase).copied();
        let ratio = reference.map_or_else(|| "-".into(), |value| Reference::ratio(row.time, value.time));
        let wall_ratio = reference.map_or_else(|| "-".into(), |value| Reference::ratio(row.wall, value.wall));
        if self
            .checksums
            .insert(phase, row.checksum)
            .is_some_and(|value| value != row.checksum)
        {
            return Err(format!("checksum divergence: {}/{}", row.arch, row.phase));
        }
        println!(
            "{},{},{},{},{},{},{},{},{},{},{}",
            row.provider,
            row.arch,
            row.phase,
            row.execution,
            row.diagnostics,
            row.context,
            row.time,
            row.wall,
            ratio,
            wall_ratio,
            row.checksum
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Output, Reference, Report, Row};
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
        assert_eq!(parsed[0].diagnostics, "unspecified");
        assert_eq!(parsed[0].context, "unspecified");
    }

    #[test]
    fn checksum_divergence() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("results.csv");
        fs::write(
            &path,
            "provider,arch,phase,time_us,checksum,a,b,c\n\
             native,aarch64,syscall,10,42,0,0,0\n\
             rust,aarch64,syscall,11,43,0,0,0\n",
        )
        .expect("write fixture");

        let error = Report {
            baseline: "native".into(),
            paths: vec![path],
        }
        .write()
        .expect_err("checksum must diverge");
        assert_eq!(error, "checksum divergence: aarch64/syscall");
    }

    #[test]
    fn balanced_cycles_are_aggregated_before_ratios() {
        fn row(provider: &str, time: u64, wall: u64) -> Row {
            Row {
                provider: provider.into(),
                arch: "aarch64".into(),
                phase: "compute".into(),
                time,
                checksum: 42,
                wall,
                execution: "verified".into(),
                diagnostics: "off".into(),
                context: "full".into(),
            }
        }

        let rows = vec![
            row("native", 10, 12),
            row("c", 30, 32),
            row("rust", 20, 22),
            row("c", 35, 37),
            row("rust", 40, 42),
            row("native", 100, 102),
            row("rust", 60, 62),
            row("native", 1000, 1002),
            row("c", 300, 302),
        ];

        let summaries = Report::summaries(rows).expect("summarize balanced cycles");
        assert_eq!(summaries.len(), 3);
        let output = Output::new("native".into(), &summaries);
        let reference = output
            .references
            .get(&("aarch64".into(), "compute".into(), "full".into()))
            .expect("aggregated native reference");
        assert_eq!(reference.time, 100);
        assert_eq!(reference.wall, 102);
        let rust = summaries
            .iter()
            .find(|row| row.provider == "rust")
            .expect("aggregated Rust sample");
        assert_eq!(rust.time, 40);
        assert_eq!(Reference::ratio(rust.time, reference.time), "0.400");
    }
}
