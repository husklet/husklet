use super::{
    BTreeMap, Capture, DIAGNOSTIC_CAPTURE, DIAGNOSTIC_OUTPUT, Duration, Entry, Error, ExitStatus, Invocation,
    Measurement, Session,
};

pub(super) fn stdout_contains(stdout: &[u8], marker: &[u8]) -> bool {
    marker.is_empty() || stdout.windows(marker.len()).any(|window| window == marker)
}

pub(super) fn output_excerpt(bytes: &[u8]) -> String {
    let shown = bytes.len().min(DIAGNOSTIC_CAPTURE);
    let suffix = bytes
        .len()
        .checked_sub(shown)
        .filter(|omitted| *omitted > 0)
        .map_or_else(String::new, |omitted| format!(" ... [{omitted} bytes omitted]"));
    let mut output = format!("{:?}{suffix}", &bytes[..shown]);
    if output.len() > DIAGNOSTIC_OUTPUT {
        const TRUNCATED: &str = " ... [excerpt truncated]";
        output.truncate(DIAGNOSTIC_OUTPUT - TRUNCATED.len());
        output.push_str(TRUNCATED);
    }
    output
}

impl Invocation<'_> {
    pub(super) async fn wait(&self, output: &mut Session) -> std::result::Result<ExitStatus, Error> {
        let timeout = Duration::from_secs(self.case.timeout);
        let deadline = tokio::time::Instant::now() + timeout;
        let waiting = self.containers.wait(&self.name);
        tokio::pin!(waiting);
        let mut captured = 0;
        loop {
            tokio::select! {
                entry = output.next() => match entry? {
                    Some(entry) => captured = capture_size(captured, &entry)?,
                    None => return tokio::time::timeout_at(deadline, waiting)
                        .await
                        .map_err(|_| timeout_error(self.case.timeout))?
                        .map_err(Into::into),
                },
                status = &mut waiting => {
                    let status = status?;
                    capture_until(output, captured, deadline, self.case.timeout).await?;
                    return Ok(status);
                }
                () = tokio::time::sleep_until(deadline) => return Err(timeout_error(self.case.timeout)),
            }
        }
    }
}

async fn capture_until(
    output: &mut Session,
    mut captured: usize,
    deadline: tokio::time::Instant,
    timeout: u64,
) -> std::result::Result<(), Error> {
    loop {
        let entry = tokio::time::timeout_at(deadline, output.next())
            .await
            .map_err(|_| timeout_error(timeout))??;
        let Some(entry) = entry else { return Ok(()) };
        captured = capture_size(captured, &entry)?;
    }
}

pub(super) fn timeout_error(seconds: u64) -> Error {
    format!("timed out after {seconds} seconds").into()
}

pub(super) fn capture_size(captured: usize, entry: &Entry) -> std::result::Result<usize, Error> {
    let captured = captured
        .checked_add(entry.bytes.len())
        .ok_or("captured output size overflow")?;
    if captured > Capture::LIMIT {
        Err(format!("captured output exceeded {} bytes", Capture::LIMIT).into())
    } else {
        Ok(captured)
    }
}

pub(super) struct Measurements {
    cold: Option<u128>,
    samples: Vec<u128>,
    phases: BTreeMap<String, (u64, Vec<u128>)>,
    lifecycle: BTreeMap<String, Vec<u128>>,
    cold_lifecycle: BTreeMap<String, u128>,
}

impl Measurements {
    pub(super) fn new(samples: u32) -> Self {
        Self {
            cold: None,
            samples: Vec::with_capacity(samples as usize),
            phases: BTreeMap::new(),
            lifecycle: BTreeMap::new(),
            cold_lifecycle: BTreeMap::new(),
        }
    }

    pub(super) fn record(
        &mut self,
        repetition: u32,
        warmups: u32,
        elapsed: u128,
        phases: Vec<(String, u128, u64)>,
        lifecycle: Vec<(String, u128)>,
    ) -> std::result::Result<(), Error> {
        if repetition == 0 {
            self.cold = Some(elapsed);
            self.cold_lifecycle.extend(lifecycle);
            return Ok(());
        }
        if repetition <= warmups {
            return Ok(());
        }
        self.samples.push(elapsed);
        for (name, time, checksum) in phases {
            self.record_phase(&name, time, checksum)?;
        }
        for (name, time) in lifecycle {
            self.lifecycle.entry(name).or_default().push(time);
        }
        Ok(())
    }

    fn record_phase(&mut self, name: &str, time: u128, checksum: u64) -> std::result::Result<(), Error> {
        let phase = self
            .phases
            .entry(name.to_owned())
            .or_insert_with(|| (checksum, Vec::new()));
        if phase.0 != checksum {
            return Err(format!("PHASE {name} checksum changed across samples").into());
        }
        phase.1.push(time);
        Ok(())
    }

    pub(super) fn finish(self, samples: u32) -> std::result::Result<Measurement, Error> {
        if self.phases.values().any(|(_, times)| times.len() != samples as usize) {
            return Err("PHASE set changed across samples".into());
        }
        if self.lifecycle.values().any(|times| times.len() != samples as usize) {
            return Err("LIFECYCLE set changed across samples".into());
        }
        Ok(Measurement {
            cold: self.cold.ok_or("cold benchmark sample was not run")?,
            samples: self.samples,
            phases: self
                .phases
                .into_iter()
                .map(|(name, (_, times))| (name, times))
                .collect(),
            lifecycle: self.lifecycle,
            cold_lifecycle: self.cold_lifecycle,
            setup: BTreeMap::new(),
        })
    }
}

pub(super) fn parse_phases(stdout: &[u8]) -> std::result::Result<Vec<(String, u128, u64)>, Error> {
    let text = std::str::from_utf8(stdout).map_err(|_| "benchmark stdout is not UTF-8")?;
    text.lines()
        .filter(|line| line.starts_with("PHASE "))
        .map(|line| {
            let mut fields = line.split_whitespace();
            let _protocol = fields.next();
            let name = fields.next().ok_or_else(|| format!("invalid PHASE row {line:?}"))?;
            let time = fields
                .next()
                .and_then(|field| field.strip_prefix("us="))
                .ok_or_else(|| format!("invalid PHASE time {line:?}"))?
                .parse::<u128>()?;
            let checksum = fields
                .next()
                .and_then(|field| field.strip_prefix("ok="))
                .ok_or_else(|| format!("invalid PHASE checksum {line:?}"))?
                .parse::<u64>()?;
            // Every phase counts the work it completed, so zero is a silent no-op, not a pass.
            if checksum == 0 {
                return Err(format!("PHASE {name} completed no work (ok=0)").into());
            }
            crate::benchmark::timebase_verdict(name, u64::try_from(time).unwrap_or(u64::MAX), checksum)?;
            Ok((name.to_owned(), time, checksum))
        })
        .collect()
}
