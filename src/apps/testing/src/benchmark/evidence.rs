use super::{definition::Campaign, schedule::Step};
use crate::{platform::HostProcess, record::FramedIdentity, suite::Error};
use fs2::FileExt as _;
#[path = "evidence_model.rs"]
mod model;
pub(in crate::benchmark) use model::{HostLoad, Phase, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn sample(
    campaign: &Campaign,
    step: &Step,
) -> Result<(BTreeMap<String, Phase>, String, String, Option<String>), Error> {
    let arm = &campaign.arms[&step.arm];
    let workload = &campaign.workloads[&step.workload];
    let guest = &workload.commands[&step.layout];
    let executable = campaign.guest(&step.arm, Path::new(&guest[0]))?;
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
    let parsed = (|| {
        let text = std::str::from_utf8(&output.stdout)?;
        require_line_framing(text)?;
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
        let expected = &campaign.workloads[&step.workload].layout_phases[&step.layout];
        let observed = phases.keys().map(String::as_str).collect::<BTreeSet<_>>();
        let expected = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if observed != expected {
            return Err(format!(
                "layout {} emitted an incomplete phase set: expected={expected:?} observed={observed:?}",
                step.layout
            )
            .into());
        }
        let frame = canonical.join("\n");
        let identity = FramedIdentity::of(frame.as_bytes());
        Ok((phases, identity, frame, None))
    })();
    parsed.map_err(|error| parse_failure(step, output.status.to_string(), &output.stdout, &output.stderr, error))
}

const FAILURE_EXCERPT: usize = 4096;

fn parse_failure(step: &Step, status: String, stdout: &[u8], stderr: &[u8], error: Error) -> Error {
    fn evidence(bytes: &[u8]) -> String {
        let excerpt = &bytes[..bytes.len().min(FAILURE_EXCERPT)];
        format!(
            "bytes={} sha256={} excerpt={:?}",
            bytes.len(),
            FramedIdentity::of(bytes),
            String::from_utf8_lossy(excerpt)
        )
    }
    format!(
        "benchmark parse failed for {} status={status}: {error}; stdout {}; stderr {}",
        step.key(),
        evidence(stdout),
        evidence(stderr)
    )
    .into()
}

fn require_line_framing(text: &str) -> Result<(), Error> {
    if !text.ends_with('\n') || text.contains('\r') {
        return Err("benchmark output is not canonically LF-framed".into());
    }
    Ok(())
}
fn require_metadata(seen: bool) -> Result<(), Error> {
    seen.then_some(())
        .ok_or_else(|| "benchmark output omitted metadata".into())
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
    let metadata = metadata_line(line).ok_or_else(|| format!("unaccounted benchmark output {line:?}"))??;
    if *metadata_seen {
        return Err("duplicate benchmark metadata".into());
    }
    *metadata_seen = true;
    canonical.push(metadata);
    Ok(())
}
fn metadata_line(line: &str) -> Option<Result<String, Error>> {
    line.strip_prefix("META ")
        .map(|_| Ok(line.to_owned()))
        .or_else(|| counter_metadata(line).map(|result| result.map(|()| line.to_owned())))
}

fn parse_phase(
    campaign: &Campaign,
    step: &Step,
    wall_us: u64,
    rest: &str,
    phases: &mut BTreeMap<String, Phase>,
    canonical: &mut Vec<String>,
) -> Result<(), Error> {
    let (name, measured, ok) = phase_fields(rest)?;
    let timed_by_wall = campaign.workloads[&step.workload].wall_time
        && campaign.workloads[&step.workload]
            .phases
            .iter()
            .any(|phase| phase == name);
    let us = if timed_by_wall { wall_us } else { measured };
    if phases
        .insert(name.to_owned(), Phase { us, ok: ok.to_owned() })
        .is_some()
    {
        return Err(format!("duplicate PHASE {name}").into());
    }
    canonical.push(format!("PHASE {name} us=<time> ok={ok}"));
    Ok(())
}

fn phase_fields(rest: &str) -> Result<(&str, u64, &str), Error> {
    let fields = rest.split_ascii_whitespace().collect::<Vec<_>>();
    let [name, micros, ok] = fields.as_slice() else {
        return Err("PHASE must be exactly `<name> us=<u64> ok=<value>`".into());
    };
    let micros = micros
        .strip_prefix("us=")
        .ok_or("PHASE second field must be us")?
        .parse::<u64>()?;
    let ok = ok.strip_prefix("ok=").ok_or("PHASE third field must be ok")?;
    if name.is_empty() || ok.is_empty() {
        return Err("PHASE name and ok must be nonempty".into());
    }
    Ok((name, micros, ok))
}

fn counter_metadata(line: &str) -> Option<Result<(), Error>> {
    let rest = line.strip_prefix("cntfrq=")?;
    let Some((frequency, divisor)) = rest.split_once(" divisor=") else {
        return Some(Err("malformed counter-frequency metadata".into()));
    };
    Some(
        frequency
            .parse::<u64>()
            .and_then(|_| divisor.parse::<u32>().map(|_| ()))
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
    let mut host_load = Vec::new();
    for _ in sample_ordinals(campaign.samples_per_row) {
        let before = load()?;
        readings.push(sample(campaign, step)?);
        host_load.push(HostLoad { before, after: load()? });
    }
    let output = readings[0].1.clone();
    let output_frame = readings[0].2.clone();
    let diagnostic = readings[0].3.clone();
    if readings
        .iter()
        .any(|(_, identity, frame, observed)| *identity != output || *frame != output_frame || *observed != diagnostic)
    {
        return Err("repeated sample exact-output mismatch".into());
    }
    let mut phases = BTreeMap::new();
    for name in readings[0].0.keys() {
        let ok = readings[0].0[name].ok.clone();
        if readings.iter().any(|(sample, _, _, _)| sample[name].ok != ok) {
            return Err(format!("repeated checksum mismatch for {name}").into());
        }
        let us = readings
            .iter()
            .map(|(sample, _, _, _)| sample[name].us)
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
        diagnostic,
        phases,
        host_load,
    })
}

fn sample_ordinals(samples_per_row: u32) -> std::ops::Range<u32> {
    0..samples_per_row
}

fn load() -> Result<f64, Error> {
    let value = fs::read_to_string("/proc/loadavg")?;
    let load = value
        .split_ascii_whitespace()
        .next()
        .ok_or("host load record is empty")?
        .parse::<f64>()?;
    if !load.is_finite() || load < 0.0 {
        return Err("host load record is invalid".into());
    }
    Ok(load)
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

#[cfg(test)]
mod tests {
    use super::{
        Measurement, counter_metadata, metadata_line, parse_failure, phase_fields, require_line_framing,
        require_metadata,
    };
    use crate::benchmark::schedule::Step;
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
    fn incomplete_phase_failure_carries_bounded_raw_evidence() {
        let step = Step {
            workload: "sqlite".into(),
            layout: "sqlite".into(),
            cell: "RR".into(),
            round: 3,
            position: 0,
            arm: "R".into(),
        };
        let stdout = [b'M'; 5000];
        let error = parse_failure(
            &step,
            "exit status: 0".into(),
            &stdout,
            b"retained diagnostic\n",
            "incomplete phase set".into(),
        )
        .to_string();
        assert!(error.contains("sqlite|sqlite|RR|3|0"));
        assert!(error.contains("status=exit status: 0"));
        assert!(error.contains("bytes=5000 sha256="));
        assert!(error.contains("incomplete phase set"));
        assert!(error.len() < 9000, "raw evidence was not bounded");
    }

    #[test]
    fn exact_output_requires_canonical_lf_byte_framing() {
        require_line_framing("META guest=plain\nPHASE malloc us=1 ok=1\n").unwrap();
        assert!(require_line_framing("META guest=plain\nPHASE malloc us=1 ok=1").is_err());
        assert!(require_line_framing("META guest=plain\r\nPHASE malloc us=1 ok=1\r\n").is_err());
    }

    #[test]
    fn phase_output_has_one_exact_field_order() {
        assert_eq!(phase_fields("malloc us=42 ok=7").unwrap(), ("malloc", 42, "7"));
        for invalid in [
            "malloc ok=7 us=42",
            "malloc us=42 us=43 ok=7",
            "malloc us=42 ok=7 extra=1",
            "malloc us=42 ok=",
        ] {
            assert!(phase_fields(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn legacy_metadata_is_validated_without_rewriting_its_bytes() {
        assert!(counter_metadata("cntfrq=1000000 divisor=20").unwrap().is_ok());
        assert!(counter_metadata("cntfrq=bad divisor=20").unwrap().is_err());

        assert_ne!(
            metadata_line("cntfrq=1000000 divisor=20").unwrap().unwrap(),
            metadata_line("META cntfrq=1000000 divisor=20").unwrap().unwrap()
        );
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

    #[test]
    fn configured_sample_count_controls_exact_measurement_attempts() {
        assert_eq!(super::sample_ordinals(3).count(), 3);
        assert_eq!(super::sample_ordinals(5).count(), 5);
    }
}
