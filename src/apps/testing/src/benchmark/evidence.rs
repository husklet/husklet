use super::{
    definition::{Campaign, GuestPath},
    schedule::{self, Step},
};
use crate::{record::FramedIdentity, suite::Error};
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
    pub phases: BTreeMap<String, Phase>,
    pub host_load: String,
}

pub(super) fn sample(campaign: &Campaign, step: &Step) -> Result<(BTreeMap<String, Phase>, String), Error> {
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
    let mut command = Command::new(&arm.command[0]);
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
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PHASE ") {
            let mut words = rest.split_ascii_whitespace();
            let name = words.next().ok_or("PHASE omitted its name")?;
            let mut micros = None;
            let mut ok = None;
            for word in words {
                if let Some(value) = word.strip_prefix("us=") {
                    micros = Some(value.parse::<u64>()?);
                } else if let Some(value) = word.strip_prefix("ok=") {
                    ok = Some(value.to_owned());
                } else {
                    return Err(format!("unaccounted PHASE field {word:?}").into());
                }
            }
            let mut us = micros.ok_or("PHASE omitted us")?;
            if campaign.workloads[&step.workload].wall_time
                && campaign.workloads[&step.workload]
                    .phases
                    .iter()
                    .any(|phase| phase == name)
            {
                us = wall_us;
            }
            let ok = ok.ok_or("PHASE omitted ok")?;
            if phases.insert(name.to_owned(), Phase { us, ok: ok.clone() }).is_some() {
                return Err(format!("duplicate PHASE {name}").into());
            }
            canonical.push(format!("PHASE {name} us=<time> ok={ok}"));
        } else if line.starts_with("META ") {
            canonical.push(line.to_owned());
        } else {
            return Err(format!("unaccounted benchmark output {line:?}").into());
        }
    }
    let expected = if step.workload == "python" {
        &campaign.workloads[&step.workload].phases
    } else {
        &campaign.layouts[&step.layout].phases
    };
    if phases.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected.iter().map(String::as_str).collect() {
        return Err(format!("layout {} emitted an incomplete phase set", step.layout).into());
    }
    let identity = FramedIdentity::of(canonical.join("\n").as_bytes());
    Ok((phases, identity))
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
    if readings.iter().any(|(_, identity)| *identity != output) {
        return Err("repeated sample exact-output mismatch".into());
    }
    let mut phases = BTreeMap::new();
    for name in readings[0].0.keys() {
        let ok = readings[0].0[name].ok.clone();
        if readings.iter().any(|(sample, _)| sample[name].ok != ok) {
            return Err(format!("repeated checksum mismatch for {name}").into());
        }
        let us = readings
            .iter()
            .map(|(sample, _)| sample[name].us)
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
        let intent = lock("/var/tmp/husklet-box.wanted", timeout)?;
        sustained_quiet(quiet, timeout, max_load)?;
        let box_lock = lock("/var/tmp/husklet-box.lock", timeout)?;
        Ok(Self {
            _intent: intent,
            _box_lock: box_lock,
        })
    }
}

fn lock(path: &str, timeout: u64) -> Result<File, Error> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while file.try_lock_exclusive().is_err() {
        if Instant::now() >= deadline {
            return Err(format!("timed out acquiring {path}").into());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(file)
}

fn sustained_quiet(seconds: u64, timeout: u64, max_load: f64) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut quiet_since = None;
    while Instant::now() < deadline {
        let load = fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|v| v.split_ascii_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(f64::INFINITY);
        let busy = [("testing", 1_u64), ("cargo", 0), ("hl-aarch64", 0), ("hl-x86_64", 0)]
            .iter()
            .any(|(name, allowance)| process_count(name) > *allowance);
        if busy || load > max_load {
            quiet_since = None;
        } else if quiet_since.is_none() {
            quiet_since = Some(Instant::now());
        } else if quiet_since.is_some_and(|start| start.elapsed() >= Duration::from_secs(seconds)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(5.min(seconds.max(1))));
    }
    Err("box did not remain quiet before measurement timeout".into())
}

fn process_count(name: &str) -> u64 {
    Command::new("pgrep")
        .args(["-cx", name])
        .output()
        .ok()
        .and_then(|output| std::str::from_utf8(&output.stdout).ok()?.trim().parse().ok())
        .unwrap_or(0)
}

pub(super) struct Ledger {
    directory: PathBuf,
    writer: BufWriter<File>,
    rows: BTreeMap<String, Row>,
    planned: BTreeSet<String>,
}

impl Ledger {
    pub fn open(directory: &Path, campaign: &Campaign, resume: bool) -> Result<Self, Error> {
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
        let planned: BTreeSet<String> = schedule::measurements(campaign)
            .into_iter()
            .map(|step| step.key())
            .collect();
        let mut rows = BTreeMap::new();
        if raw.exists() {
            for line in BufReader::new(File::open(&raw)?).lines() {
                let row: Row = serde_json::from_str(&line?)?;
                if !planned.contains(&row.key) || rows.insert(row.key.clone(), row).is_some() {
                    return Err("ledger has a foreign or duplicate row".into());
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
        serde_json::to_writer(&mut self.writer, row)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        self.rows.insert(row.key.clone(), row.clone());
        Ok(())
    }
    pub fn complete(&self) -> Result<Vec<Row>, Error> {
        if self.rows.keys().collect::<BTreeSet<_>>() != self.planned.iter().collect() {
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
