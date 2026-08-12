use super::{
    definition::{Campaign, GuestPath},
    schedule::{self, Step},
};
use crate::{platform::HostProcess, record::FramedIdentity, suite::Error};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, BufWriter, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Phase {
    pub us: u64,
    pub ok: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Row {
    pub key: String,
    pub workload: String,
    pub layout: String,
    pub cell: String,
    pub round: u32,
    pub position: usize,
    pub arm: String,
    pub output: String,
    pub output_frame: String,
    pub phases: BTreeMap<String, Phase>,
    pub host_load: String,
}

pub(super) fn sample(campaign: &Campaign, step: &Step) -> Result<(BTreeMap<String, Phase>, String, String), Error> {
    let arm = &campaign.arms[&step.arm];
    let workload = &campaign.workloads[&step.workload];
    let guest = &workload.commands[&step.layout];
    let executable = match arm.guest_path {
        GuestPath::HostAbsolute => guest[0].clone(),
        GuestPath::RootfsAbsolute => format!(
            "/{}",
            Path::new(&guest[0]).strip_prefix(&campaign.rootfs.path)?.display()
        ),
    };
    let mut command = HostProcess::standard(&arm.command[0]);
    command
        .args(&arm.command[1..])
        .arg(executable)
        .args(&guest[1..])
        .stdin(Stdio::null());
    let started = Instant::now();
    let output = bounded_output(&mut command, Duration::from_secs(workload.timeout_seconds))?;
    let wall_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX).max(1);
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "benchmark {}/{}/{} failed: status={} stderr={}",
            step.workload,
            step.layout,
            step.arm,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let text = std::str::from_utf8(&output.stdout)?;
    let mut phases = BTreeMap::new();
    let mut canonical = Vec::new();
    let mut metadata_seen = false;
    for line in text.lines() {
        parse_output_line(
            campaign,
            step,
            wall_us,
            line,
            &mut phases,
            &mut canonical,
            &mut metadata_seen,
        )?;
    }
    require_metadata(metadata_seen)?;
    let expected = if step.workload == "python" {
        &campaign.workloads[&step.workload].phases
    } else {
        &campaign.layouts[&step.layout].phases
    };
    if phases.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected.iter().map(String::as_str).collect() {
        return Err(format!("layout {} emitted an incomplete phase set", step.layout).into());
    }
    let frame = canonical.join("\n");
    let identity = FramedIdentity::of(frame.as_bytes());
    Ok((phases, identity, frame))
}

fn require_metadata(seen: bool) -> Result<(), Error> {
    if seen {
        Ok(())
    } else {
        Err("benchmark output omitted metadata".into())
    }
}

fn parse_output_line(
    campaign: &Campaign,
    step: &Step,
    wall_us: u64,
    line: &str,
    phases: &mut BTreeMap<String, Phase>,
    canonical: &mut Vec<String>,
    metadata_seen: &mut bool,
) -> Result<(), Error> {
    if let Some(rest) = line.strip_prefix("PHASE ") {
        return parse_phase(campaign, step, wall_us, rest, phases, canonical);
    }
    let metadata = line
        .strip_prefix("META ")
        .map(|_| Ok(line.to_owned()))
        .or_else(|| counter_metadata(line))
        .ok_or_else(|| format!("unaccounted benchmark output {line:?}"))??;
    if *metadata_seen {
        return Err("duplicate benchmark metadata".into());
    }
    *metadata_seen = true;
    canonical.push(metadata);
    Ok(())
}

fn parse_phase(
    campaign: &Campaign,
    step: &Step,
    wall_us: u64,
    rest: &str,
    phases: &mut BTreeMap<String, Phase>,
    canonical: &mut Vec<String>,
) -> Result<(), Error> {
    let mut words = rest.split_ascii_whitespace();
    let name = words.next().ok_or("PHASE omitted its name")?;
    let mut micros = None;
    let mut ok = None;
    for word in words {
        match word.split_once('=') {
            Some(("us", value)) => micros = Some(value.parse::<u64>()?),
            Some(("ok", value)) => ok = Some(value.to_owned()),
            _ => return Err(format!("unaccounted PHASE field {word:?}").into()),
        }
    }
    let measured = micros.ok_or("PHASE omitted us")?;
    let timed_by_wall = campaign.workloads[&step.workload].wall_time
        && campaign.workloads[&step.workload]
            .phases
            .iter()
            .any(|phase| phase == name);
    let us = if timed_by_wall { wall_us } else { measured };
    let ok = ok.ok_or("PHASE omitted ok")?;
    if phases.insert(name.to_owned(), Phase { us, ok: ok.clone() }).is_some() {
        return Err(format!("duplicate PHASE {name}").into());
    }
    canonical.push(format!("PHASE {name} us=<time> ok={ok}"));
    Ok(())
}

fn counter_metadata(line: &str) -> Option<Result<String, Error>> {
    let rest = line.strip_prefix("cntfrq=")?;
    let Some((frequency, divisor)) = rest.split_once(" divisor=") else {
        return Some(Err("malformed counter-frequency metadata".into()));
    };
    Some(
        frequency
            .parse::<u64>()
            .and_then(|_| {
                divisor
                    .parse::<u32>()
                    .map(|_| format!("META cntfrq={frequency} divisor={divisor}"))
            })
            .map_err(|_| "malformed counter-frequency metadata".into()),
    )
}

fn bounded_output(command: &mut Command, timeout: Duration) -> Result<std::process::Output, Error> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Err(format!("benchmark sample exceeded {} seconds", timeout.as_secs()).into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn measure(campaign: &Campaign, step: &Step) -> Result<Row, Error> {
    let mut readings = Vec::new();
    for _ in 0..campaign.samples_per_row {
        readings.push(sample(campaign, step)?);
    }
    let output = readings[0].1.clone();
    let output_frame = readings[0].2.clone();
    if readings
        .iter()
        .any(|(_, identity, frame)| *identity != output || *frame != output_frame)
    {
        return Err("repeated sample exact-output mismatch".into());
    }
    let mut phases = BTreeMap::new();
    for name in readings[0].0.keys() {
        let ok = readings[0].0[name].ok.clone();
        if readings.iter().any(|(sample, _, _)| sample[name].ok != ok) {
            return Err(format!("repeated checksum mismatch for {name}").into());
        }
        let us = readings
            .iter()
            .map(|(sample, _, _)| sample[name].us)
            .min()
            .ok_or("sample set is empty")?;
        phases.insert(name.clone(), Phase { us, ok });
    }
    Ok(Row {
        key: step.key(),
        workload: step.workload.clone(),
        layout: step.layout.clone(),
        cell: step.cell.clone(),
        round: step.round,
        position: step.position,
        arm: step.arm.clone(),
        output,
        output_frame,
        phases,
        host_load: load(),
    })
}

fn load() -> String {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|value| value.split_ascii_whitespace().next().map(str::to_owned))
        .unwrap_or_else(|| "unavailable".into())
}

pub(super) struct Measurement {
    _intent: File,
    _box_lock: File,
}

impl Measurement {
    pub fn acquire(quiet: u64, timeout: u64, max_load: f64) -> Result<Self, Error> {
        Self::acquire_with(
            Path::new("/var/tmp/husklet-box.wanted"),
            Path::new("/var/tmp/husklet-box.lock"),
            Duration::from_secs(quiet),
            Duration::from_secs(timeout),
            |lock_held| host_quiet(max_load, Path::new("/var/tmp/husklet-box.lock"), lock_held),
        )
    }

    fn acquire_with(
        intent_path: &Path,
        box_path: &Path,
        quiet: Duration,
        timeout: Duration,
        mut probe: impl FnMut(bool) -> Result<bool, Error>,
    ) -> Result<Self, Error> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("measurement acquisition timeout overflowed")?;
        let intent = lock(intent_path, deadline)?;
        drop(open_lock(box_path)?);
        sustained_quiet(quiet, deadline, &mut probe)?;
        let box_lock = lock(box_path, deadline)?;
        if !probe(true)? {
            return Err("box became busy while acquiring the measurement lock".into());
        }
        Ok(Self {
            _intent: intent,
            _box_lock: box_lock,
        })
    }
}

fn lock(path: &Path, deadline: Instant) -> Result<File, Error> {
    let file = open_lock(path)?;
    while file.try_lock_exclusive().is_err() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(format!("timed out acquiring {}", path.display()).into());
        };
        std::thread::sleep(remaining.min(Duration::from_secs(1)));
    }
    Ok(file)
}

fn open_lock(path: &Path) -> Result<File, Error> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?)
}

fn sustained_quiet(
    quiet: Duration,
    deadline: Instant,
    probe: &mut impl FnMut(bool) -> Result<bool, Error>,
) -> Result<(), Error> {
    let mut quiet_since = None;
    while Instant::now() < deadline {
        if !probe(false)? {
            quiet_since = None;
        } else if quiet.is_zero() {
            return Ok(());
        } else if quiet_since.is_none() {
            quiet_since = Some(Instant::now());
        } else if quiet_since.is_some_and(|start| start.elapsed() >= quiet) {
            return Ok(());
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        std::thread::sleep(
            remaining
                .min(Duration::from_secs(5))
                .min(quiet.max(Duration::from_secs(1))),
        );
    }
    Err("box did not remain quiet before measurement timeout".into())
}

fn host_quiet(max_load: f64, box_path: &Path, lock_held: bool) -> Result<bool, Error> {
    let load = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|value| value.split_ascii_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(f64::INFINITY);
    let mut busy = false;
    for (name, allowance) in [("testing", 1_u64), ("cargo", 0), ("hl-aarch64", 0), ("hl-x86_64", 0)] {
        busy |= HostProcess::exact_process_count(name)? > allowance;
    }
    let allowed_holders = u64::from(lock_held);
    Ok(!busy && load <= max_load && box_lock_holder_count(box_path)? == allowed_holders)
}

#[cfg(target_os = "linux")]
fn box_lock_holder_count(path: &Path) -> Result<u64, Error> {
    let target = fs::metadata(path)?;
    let mut holders = 0_u64;
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if !process.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        holders += process_lock_holders(&process, &target)?;
    }
    Ok(holders)
}

#[cfg(target_os = "linux")]
fn process_lock_holders(process: &fs::DirEntry, target: &fs::Metadata) -> Result<u64, Error> {
    let descriptors = match fs::read_dir(process.path().join("fd")) {
        Ok(descriptors) => descriptors,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(0);
        }
        Err(error) => return Err(error.into()),
    };
    let mut holders = 0;
    for descriptor in descriptors {
        holders += u64::from(descriptor_matches_lock(descriptor, target)?);
    }
    Ok(holders)
}

#[cfg(target_os = "linux")]
fn descriptor_matches_lock(descriptor: std::io::Result<fs::DirEntry>, target: &fs::Metadata) -> Result<bool, Error> {
    use std::os::unix::fs::MetadataExt as _;

    let descriptor = match descriptor {
        Ok(descriptor) => descriptor,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match fs::metadata(descriptor.path()) {
        Ok(metadata) => Ok(metadata.dev() == target.dev() && metadata.ino() == target.ino()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(target_os = "linux"))]
fn box_lock_holder_count(_path: &Path) -> Result<u64, Error> {
    Err("box-lock holder counting requires Linux procfs".into())
}

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
        admit_destination(directory, resume)?;
        let manifest = directory.join("manifest.json");
        let raw = directory.join("raw.jsonl");
        let identity = campaign.identity()?;
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
        let planned: BTreeMap<String, Step> = schedule::measurements(campaign)
            .into_iter()
            .map(|step| (step.key(), step))
            .collect();
        let mut rows = BTreeMap::new();
        if raw.exists() {
            for line in BufReader::new(File::open(&raw)?).lines() {
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
    pub fn publish(&self, report: &super::verdict::Report) -> Result<(), Error> {
        fs::write(self.directory.join("report.tsv"), &report.text)?;
        fs::write(self.directory.join("verdict.txt"), format!("{}\n", report.verdict))?;
        Ok(())
    }
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
mod ledger_tests {
    use super::{Ledger, Phase, Row, Step, admit_destination};
    use std::{
        collections::BTreeMap,
        fs::{self, File},
        io::BufWriter,
    };

    fn step() -> Step {
        Step {
            workload: "malloc".into(),
            layout: "plain".into(),
            cell: "EE".into(),
            round: 0,
            position: 0,
            arm: "E".into(),
        }
    }

    fn row(key: &str) -> Row {
        Row {
            key: key.into(),
            workload: "malloc".into(),
            layout: "plain".into(),
            cell: "EE".into(),
            round: 0,
            position: 0,
            arm: "E".into(),
            output: "same".into(),
            output_frame: "frame".into(),
            phases: [(
                "malloc".into(),
                Phase {
                    us: 1,
                    ok: "same".into(),
                },
            )]
            .into(),
            host_load: "0.1".into(),
        }
    }

    #[test]
    fn append_rejects_duplicate_and_foreign_rows_before_durable_write() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("raw.jsonl");
        let expected = step();
        let key = expected.key();
        let mut ledger = Ledger {
            directory: directory.path().into(),
            writer: BufWriter::new(File::create(&raw).unwrap()),
            rows: BTreeMap::new(),
            planned: BTreeMap::from([(key.clone(), expected)]),
        };
        ledger.append(&row(&key)).unwrap();
        let durable = fs::metadata(&raw).unwrap().len();
        assert!(ledger.append(&row(&key)).is_err());
        assert!(ledger.append(&row("foreign")).is_err());
        assert_eq!(fs::metadata(&raw).unwrap().len(), durable);
        assert_eq!(ledger.rows.keys().map(String::as_str).collect::<Vec<_>>(), [key]);
    }

    #[test]
    fn append_rejects_valid_key_with_out_of_schedule_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("raw.jsonl");
        let expected = step();
        let key = expected.key();
        let mut ledger = Ledger {
            directory: directory.path().into(),
            writer: BufWriter::new(File::create(&raw).unwrap()),
            rows: BTreeMap::new(),
            planned: BTreeMap::from([(key.clone(), expected)]),
        };
        let mut forged = row(&key);
        forged.arm = "I".into();
        let error = ledger.append(&forged).unwrap_err();
        assert!(error.to_string().contains("violates the schedule"), "{error}");
        assert_eq!(fs::metadata(raw).unwrap().len(), 0);
        assert!(ledger.rows.is_empty());
    }

    #[test]
    fn complete_rejects_an_incomplete_rust_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("raw.jsonl");
        let expected = step();
        let ledger = Ledger {
            directory: directory.path().into(),
            writer: BufWriter::new(File::create(raw).unwrap()),
            rows: BTreeMap::new(),
            planned: BTreeMap::from([(expected.key(), expected)]),
        };
        let Err(error) = ledger.complete() else {
            panic!("incomplete ledger was accepted");
        };
        assert!(error.to_string().contains("incomplete"));
    }

    #[test]
    fn completed_result_directory_cannot_be_replayed_as_resume() {
        let directory = tempfile::tempdir().unwrap();
        admit_destination(directory.path(), true).unwrap();
        fs::write(directory.path().join("report.tsv"), "PASS\n").unwrap();
        let error = admit_destination(directory.path(), true).unwrap_err();
        assert!(error.to_string().contains("already published"), "{error}");
        // A non-resume run still reaches create_dir, which independently enforces uniqueness.
        admit_destination(directory.path(), false).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::{Measurement, require_metadata};
    use std::{
        cell::Cell,
        fs::OpenOptions,
        time::{Duration, Instant},
    };

    #[test]
    fn exact_output_requires_identity_metadata() {
        require_metadata(true).unwrap();
        assert!(require_metadata(false).is_err());
    }

    #[test]
    fn quiet_is_rechecked_while_the_box_lock_is_held() {
        let directory = tempfile::tempdir().unwrap();
        let intent = directory.path().join("wanted");
        let box_lock = directory.path().join("box");
        let probes = Cell::new(0);
        let result = Measurement::acquire_with(
            &intent,
            &box_lock,
            Duration::ZERO,
            Duration::from_secs(1),
            |lock_held| {
                probes.set(probes.get() + 1);
                if probes.get() == 1 {
                    assert!(!lock_held);
                    return Ok(true);
                }
                assert!(lock_held);
                let competing = OpenOptions::new().read(true).write(true).open(&box_lock).unwrap();
                assert!(fs2::FileExt::try_lock_shared(&competing).is_err());
                Ok(false)
            },
        );
        let Err(error) = result else {
            panic!("measurement accepted a busy post-lock probe");
        };
        assert!(error.to_string().contains("became busy"));
        assert_eq!(probes.get(), 2);
    }

    #[test]
    fn acquisition_timeout_is_one_deadline_across_quiet_and_box_lock() {
        let directory = tempfile::tempdir().unwrap();
        let intent = directory.path().join("wanted");
        let box_path = directory.path().join("box");
        let competing = super::open_lock(&box_path).unwrap();
        fs2::FileExt::lock_shared(&competing).unwrap();
        let started = Instant::now();
        let result = Measurement::acquire_with(&intent, &box_path, Duration::ZERO, Duration::from_millis(200), |_| {
            std::thread::sleep(Duration::from_millis(150));
            Ok(true)
        });
        let Err(error) = result else {
            panic!("measurement acquired a lock held by a competitor");
        };
        assert!(error.to_string().contains("timed out acquiring"));
        assert!(started.elapsed() < Duration::from_millis(300));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn holder_count_observes_the_box_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("box");
        let held = super::open_lock(&path).unwrap();
        assert_eq!(super::box_lock_holder_count(&path).unwrap(), 1);
        fs2::FileExt::lock_shared(&held).unwrap();
        assert_eq!(super::box_lock_holder_count(&path).unwrap(), 1);
        drop(held);
        assert_eq!(super::box_lock_holder_count(&path).unwrap(), 0);
    }
}
