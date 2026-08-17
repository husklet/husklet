use super::{
    definition::Campaign,
    evidence::Row,
    schedule::{self, Step},
    verdict::Report,
};
use crate::suite::Error;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, BufWriter, Write as _},
    path::{Path, PathBuf},
};

pub(super) struct Ledger {
    directory: PathBuf,
    writer: BufWriter<File>,
    rows: BTreeMap<String, Row>,
    planned: BTreeMap<String, Step>,
}

fn follows_schedule(row: &Row, step: &Step) -> bool {
    row.key == step.key()
        && row.workload == step.workload
        && row.layout == step.layout
        && row.cell == step.cell
        && row.round == step.round
        && row.position == step.position
        && row.arm == step.arm
}

impl Ledger {
    pub fn open(directory: &Path, campaign: &Campaign, resume: bool) -> Result<Self, Error> {
        Self::open_planned(
            directory,
            campaign,
            resume,
            schedule::measurements(campaign),
            "acceptance",
        )
    }

    pub fn open_planned(
        directory: &Path,
        campaign: &Campaign,
        resume: bool,
        steps: Vec<Step>,
        mode: &str,
    ) -> Result<Self, Error> {
        admit_destination(directory, resume)?;
        let manifest = directory.join("manifest.json");
        let raw = directory.join("raw.jsonl");
        let identity = if mode == "acceptance" {
            campaign.identity()?
        } else {
            format!("{}:{mode}", campaign.identity()?)
        };
        if resume {
            let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&manifest)?)?;
            if recorded["identity"] != identity {
                return Err("resume campaign identity differs".into());
            }
        } else {
            fs::create_dir(directory).map_err(|error| format!("result directory must be new: {error}"))?;
            fs::write(
                &manifest,
                serde_json::to_vec_pretty(&serde_json::json!({"identity": identity, "campaign": campaign}))?,
            )?;
        }
        let planned: BTreeMap<String, Step> = steps.into_iter().map(|step| (step.key(), step)).collect();
        let mut rows = read_rows(&raw, &planned)?;
        if resume && discard_partial_pairs(&mut rows, &planned) {
            rewrite(&raw, &rows)?;
        }
        let writer = BufWriter::new(OpenOptions::new().create(true).append(true).open(raw)?);
        Ok(Self {
            directory: directory.into(),
            writer,
            rows,
            planned,
        })
    }

    pub fn contains(&self, key: &str) -> bool {
        self.rows.contains_key(key)
    }

    pub fn append(&mut self, row: &Row) -> Result<(), Error> {
        let Some(step) = self.planned.get(&row.key) else {
            return Err("benchmark ledger rejected a foreign row".into());
        };
        if !follows_schedule(row, step) {
            return Err("benchmark ledger rejected a row that violates the schedule".into());
        }
        if self.rows.contains_key(&row.key) {
            return Err("benchmark ledger rejected a duplicate row".into());
        }
        serde_json::to_writer(&mut self.writer, row)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        self.rows.insert(row.key.clone(), row.clone());
        Ok(())
    }

    pub fn complete(&self) -> Result<Vec<Row>, Error> {
        if self.rows.keys().collect::<BTreeSet<_>>() != self.planned.keys().collect() {
            return Err(format!(
                "benchmark ledger incomplete: {}/{} rows",
                self.rows.len(),
                self.planned.len()
            )
            .into());
        }
        Ok(self.rows.values().cloned().collect())
    }

    pub fn require_space(&self, gib: f64) -> Result<(), Error> {
        let free = fs2::available_space(&self.directory)? as f64 / 1024_f64.powi(3);
        if free < gib {
            return Err(format!("free disk {free:.1} GiB is below {gib:.1} GiB").into());
        }
        Ok(())
    }

    pub fn publish(&self, report: &Report) -> Result<(), Error> {
        fs::write(self.directory.join("report.tsv"), &report.text)?;
        fs::write(self.directory.join("verdict.txt"), format!("{}\n", report.verdict))?;
        Ok(())
    }
}

fn read_rows(raw: &Path, planned: &BTreeMap<String, Step>) -> Result<BTreeMap<String, Row>, Error> {
    let mut rows = BTreeMap::new();
    if !raw.exists() {
        return Ok(rows);
    }
    for line in BufReader::new(File::open(raw)?).lines() {
        let row: Row = serde_json::from_str(&line?)?;
        let Some(step) = planned.get(&row.key) else {
            return Err("ledger has a foreign row".into());
        };
        if !follows_schedule(&row, step) {
            return Err("ledger row violates the benchmark schedule".into());
        }
        if rows.insert(row.key.clone(), row).is_some() {
            return Err("ledger has a duplicate row".into());
        }
    }
    Ok(rows)
}

fn discard_partial_pairs(rows: &mut BTreeMap<String, Row>, planned: &BTreeMap<String, Step>) -> bool {
    let partial = rows
        .keys()
        .filter(|key| {
            planned[*key]
                .paired_key()
                .is_none_or(|paired| !rows.contains_key(&paired))
        })
        .cloned()
        .collect::<Vec<_>>();
    for key in &partial {
        rows.remove(key);
    }
    !partial.is_empty()
}

fn rewrite(raw: &Path, rows: &BTreeMap<String, Row>) -> Result<(), Error> {
    let parent = raw.parent().ok_or("benchmark ledger has no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    for row in rows.values() {
        serde_json::to_writer(&mut staged, row)?;
        staged.write_all(b"\n")?;
    }
    staged.flush()?;
    staged.as_file().sync_all()?;
    staged.persist(raw)?;
    Ok(())
}

fn admit_destination(directory: &Path, resume: bool) -> Result<(), Error> {
    if resume
        && ["report.tsv", "verdict.txt"]
            .iter()
            .any(|name| directory.join(name).exists())
    {
        return Err("benchmark result directory is already published; use a unique path for a new run".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::evidence::{HostLoad, Phase};

    fn step(position: usize) -> Step {
        Step {
            workload: "malloc".into(),
            layout: "plain".into(),
            cell: "EE".into(),
            round: 0,
            position,
            arm: "E".into(),
        }
    }

    fn row(step: &Step) -> Row {
        Row {
            key: step.key(),
            workload: step.workload.clone(),
            layout: step.layout.clone(),
            cell: step.cell.clone(),
            round: step.round,
            position: step.position,
            arm: step.arm.clone(),
            output: "same".into(),
            output_frame: "frame".into(),
            diagnostic: None,
            phases: [(
                "malloc".into(),
                Phase {
                    us: 1,
                    ok: "same".into(),
                },
            )]
            .into(),
            host_load: vec![HostLoad {
                before: 0.1,
                after: 0.2,
            }],
        }
    }

    fn ledger(steps: &[Step]) -> (tempfile::TempDir, Ledger) {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("raw.jsonl");
        let planned = steps.iter().cloned().map(|step| (step.key(), step)).collect();
        let ledger = Ledger {
            directory: directory.path().into(),
            writer: BufWriter::new(File::create(raw).unwrap()),
            rows: BTreeMap::new(),
            planned,
        };
        (directory, ledger)
    }

    #[test]
    fn append_rejects_duplicate_and_foreign_rows_before_durable_write() {
        let expected = step(0);
        let (directory, mut ledger) = ledger(std::slice::from_ref(&expected));
        ledger.append(&row(&expected)).unwrap();
        let raw = directory.path().join("raw.jsonl");
        let durable = fs::metadata(&raw).unwrap().len();
        assert!(ledger.append(&row(&expected)).is_err());
        let foreign = step(1);
        assert!(ledger.append(&row(&foreign)).is_err());
        assert_eq!(fs::metadata(raw).unwrap().len(), durable);
    }

    #[test]
    fn append_rejects_valid_key_with_out_of_schedule_provenance() {
        let expected = step(0);
        let (directory, mut ledger) = ledger(std::slice::from_ref(&expected));
        let mut forged = row(&expected);
        forged.arm = "I".into();
        assert!(
            ledger
                .append(&forged)
                .unwrap_err()
                .to_string()
                .contains("violates the schedule")
        );
        assert_eq!(fs::metadata(directory.path().join("raw.jsonl")).unwrap().len(), 0);
    }

    #[test]
    fn resume_discards_a_crash_interrupted_half_pair() {
        let steps = [step(0), step(1)];
        let planned = steps.iter().cloned().map(|step| (step.key(), step)).collect();
        let mut rows = BTreeMap::from([(steps[0].key(), row(&steps[0]))]);
        assert!(discard_partial_pairs(&mut rows, &planned));
        assert!(rows.is_empty());
    }

    #[test]
    fn resume_preserves_a_complete_balanced_pair() {
        let steps = [step(0), step(1)];
        let planned = steps.iter().cloned().map(|step| (step.key(), step)).collect();
        let mut rows = steps.iter().map(|step| (step.key(), row(step))).collect();
        assert!(!discard_partial_pairs(&mut rows, &planned));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn complete_rejects_an_incomplete_rust_ledger() {
        let expected = step(0);
        let (_directory, ledger) = ledger(&[expected]);
        let Err(error) = ledger.complete() else {
            panic!("incomplete benchmark ledger was accepted")
        };
        assert!(error.to_string().contains("incomplete"));
    }

    #[test]
    fn completed_result_directory_cannot_be_replayed_as_resume() {
        let directory = tempfile::tempdir().unwrap();
        admit_destination(directory.path(), true).unwrap();
        fs::write(directory.path().join("report.tsv"), "PASS\n").unwrap();
        assert!(
            admit_destination(directory.path(), true)
                .unwrap_err()
                .to_string()
                .contains("already published")
        );
        admit_destination(directory.path(), false).unwrap();
    }
}
