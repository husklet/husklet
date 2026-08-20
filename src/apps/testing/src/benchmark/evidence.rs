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
    let arm = campaign.arms[&step.arm]
        .profile(step.profile)
        .ok_or_else(|| format!("benchmark arm {} has no {} profile", step.arm, step.profile.as_str()))?;
    let workload = &campaign.workloads[&step.workload];
    let guest = &workload.commands[&step.layout];
    let executable = campaign.guest(&step.arm, step.profile, Path::new(&guest[0]))?;
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
        profile: step.profile,
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
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(format!("cannot acquire {}: {error}", path.display()).into());
            }
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(format!("timed out acquiring {}", path.display()).into());
        };
        std::thread::sleep(remaining.min(Duration::from_secs(1)));
    }
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
    // Cargo's hl-engine test executables are named `hl_engine-<hash>`. They can
    // saturate a CPU while remaining invisible to every exact-name probe above.
    #[cfg(target_os = "linux")]
    {
        busy |= process_name_prefix_count("hl_engine-")? != 0;
    }
    Ok(!busy && load <= max_load && box_lock_occupancy_matches(box_path, lock_held)?)
}

/// Answers whether the box lock is occupied exactly as this lane expects: by
/// nobody before it acquires, and by itself alone once it has.
///
/// The answer comes from the lock, not from `/proc/locks`. That table cannot
/// support the question, in three independently measured ways. It is a seq_file
/// over a per-CPU list enumerated by ordinal position, and reading it takes six
/// to ten `read` calls, each of which re-enters the list at an ordinal; a lock
/// inserted or removed ahead of that ordinal shifts every later record, so a
/// lock held for the entire read is silently skipped. Measured on this box
/// against a lock that never moved, 2000 observations per configuration: 48,
/// 245, 610 and 627 misses, and also 0 -- the rate depends on where the
/// observed record falls in an order nothing here controls, which is why no
/// bounded test can summon it. A single-call read is atomic but stops at one
/// page, 4062 bytes of a 12097-byte table, trading skipping for truncation.
/// And the table lists lock families that cannot contend at all: an OFD lock
/// on the box file prints a record against the same device and inode while a
/// second description takes `LOCK_EX` straight through it.
///
/// A `flock` attempt on an independent open file description asks the question
/// the harness actually needs answered -- can this box be taken right now -- in
/// one kernel operation, and it reaches the lock the same way an acquiring lane
/// does. That also makes it notice the one documented way to break the box
/// protocol: if the lockfile is unlinked and recreated while this lane holds it,
/// the probe opens the replacement inode, acquires, and reports the mismatch
/// instead of certifying a box this lane no longer owns.
fn box_lock_occupancy_matches(path: &Path, held_by_this_lane: bool) -> Result<bool, Error> {
    let probe = open_lock(path)?;
    // An exclusive hold already excludes every other holder, so what is worth
    // confirming is that the hold is real and still reached through this name:
    // a shared acquisition on an independent description must be refused.
    // Named through `fs2` on both arms: `File` has inherent `try_lock_shared`
    // and `unlock` of its own, and letting the two arms resolve to different
    // owners is how this pair stops type-checking.
    let attempt = if held_by_this_lane {
        fs2::FileExt::try_lock_shared(&probe)
    } else {
        fs2::FileExt::try_lock_exclusive(&probe)
    };
    match attempt {
        Ok(()) => {
            // Release on the descriptor rather than on close. A `flock` lives on
            // the open file description, so a sibling thread's `fork` between the
            // probe and its close keeps the probe's own lock registered until the
            // child execs -- measured at 1.0% of closes in a process spawning
            // children, and never without one.
            fs2::FileExt::unlock(&probe)?;
            Ok(!held_by_this_lane)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(held_by_this_lane),
        Err(error) => Err(format!("cannot probe {}: {error}", path.display()).into()),
    }
}

#[cfg(target_os = "linux")]
fn process_name_prefix_count(prefix: &str) -> Result<u64, Error> {
    let mut count = 0;
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if !process.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        match fs::read_to_string(process.path().join("comm")) {
            Ok(name) => count += u64::from(name.trim_end().starts_with(prefix)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(count)
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
            profile: crate::benchmark::definition::ProfileKind::Primary,
            paired_profile: crate::benchmark::definition::ProfileKind::Primary,
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
        assert!(error.contains("sqlite|sqlite|RR|primary|3|0"));
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

    #[test]
    fn box_occupancy_separates_an_open_descriptor_from_a_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("box");
        let held = super::open_lock(&path).unwrap();
        assert!(super::box_lock_occupancy_matches(&path, false).unwrap());
        fs2::FileExt::lock_shared(&held).unwrap();
        assert!(!super::box_lock_occupancy_matches(&path, false).unwrap());
        // Released on the descriptor, not by closing it. A `flock` belongs to the
        // open file description, so any sibling thread that forks while this one
        // holds the lock keeps it registered until the child execs; measured at
        // 1.0% of closes in a process spawning children, and 0 of 4000 without.
        fs2::FileExt::unlock(&held).unwrap();
        assert!(super::box_lock_occupancy_matches(&path, false).unwrap());
    }

    #[test]
    fn box_occupancy_refuses_a_hold_that_does_not_exclude_others() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("box");
        let held = super::open_lock(&path).unwrap();
        // A shared hold occupies the box lock exactly as an exclusive hold does,
        // and a count of holders cannot tell the two apart. A lane that acquired
        // shared would be certified as the box's sole occupant while every
        // builder on the machine remains free to join it -- which is the
        // measurement running through someone else's build.
        fs2::FileExt::lock_shared(&held).unwrap();
        assert!(!super::box_lock_occupancy_matches(&path, true).unwrap());
        fs2::FileExt::unlock(&held).unwrap();
        fs2::FileExt::lock_exclusive(&held).unwrap();
        assert!(super::box_lock_occupancy_matches(&path, true).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn box_occupancy_ignores_a_lock_family_that_cannot_contend() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("box");
        let stranger = super::open_lock(&path).unwrap();
        // An open-file-description lock is listed in `/proc/locks` against the
        // same device and inode as the box lock, and `flock` acquires straight
        // through it. Counting records would report a holder that blocks nobody.
        rustix::fs::fcntl_lock(&stranger, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        assert!(super::box_lock_occupancy_matches(&path, false).unwrap());
        let held = super::open_lock(&path).unwrap();
        fs2::FileExt::try_lock_exclusive(&held).unwrap();
        assert!(!super::box_lock_occupancy_matches(&path, false).unwrap());
    }

    #[test]
    fn box_occupancy_reports_a_lockfile_replaced_beneath_the_lane() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("box");
        let held = super::open_lock(&path).unwrap();
        fs2::FileExt::lock_exclusive(&held).unwrap();
        assert!(super::box_lock_occupancy_matches(&path, true).unwrap());
        // Deleting the path releases nothing, so the lane still holds the lock --
        // on an inode the name no longer reaches. Every later lane acquires the
        // replacement immediately and both believe they own the box.
        std::fs::remove_file(&path).unwrap();
        drop(super::open_lock(&path).unwrap());
        assert!(!super::box_lock_occupancy_matches(&path, true).unwrap());
    }

    #[test]
    fn configured_sample_count_controls_exact_measurement_attempts() {
        assert_eq!(super::sample_ordinals(3).count(), 3);
        assert_eq!(super::sample_ordinals(5).count(), 5);
    }
}
