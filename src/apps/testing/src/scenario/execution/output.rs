use super::*;

pub(super) fn unpack_regular_files(archive: &[u8]) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    let mut output = Vec::new();
    for entry in tar::Archive::new(archive).entries()? {
        let entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        entry
            .take((Capture::LIMIT - output.len()) as u64 + 1)
            .read_to_end(&mut output)?;
        if output.len() > Capture::LIMIT {
            return Err(format!("copied output exceeded {} bytes", Capture::LIMIT).into());
        }
    }
    Ok(output)
}

#[derive(Default)]
pub(super) struct LimitedOutput(pub(super) Vec<u8>);

impl std::io::Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::FileTooLarge, "copy archive size overflow"))?;
        if next > Capture::LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!("copy archive exceeded {} bytes", Capture::LIMIT),
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) async fn run_exec(
    containers: &hl_container::Containers,
    case: &Sample,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
    name: &str,
    action: &crate::scenario::definition::Step,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Error> {
    let process = crate::scenario::process::action(case, action, runtime, rootfs)?;
    let execution = containers.executions().create(name, ExecSpec::new(process)).await?;
    let mut session = containers.executions().start(&execution.id).await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(entry) = session.next().await? {
        match entry.stream {
            Stream::Stdout => stdout.extend(entry.bytes),
            Stream::Stderr => stderr.extend(entry.bytes),
        }
        crate::suite::Capture::bounded(stdout.len(), stderr.len())?;
    }
    let status = containers.executions().wait(&execution.id).await?;
    Ok((status, stdout, stderr))
}

pub(super) fn require_success(noun: &str, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> Result<(), Error> {
    if status != ExitStatus::Code(0) {
        return Err(format!("{noun} exited {status:?}; {}", output_summary(stdout, stderr)).into());
    }
    Ok(())
}

pub(super) fn verify(case: &Sample, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> Result<String, Error> {
    if status != ExitStatus::Code(case.exit) {
        return Err(format!(
            "exit {status:?}, expected {}; {}",
            case.exit,
            output_summary(stdout, stderr)
        )
        .into());
    }
    let mut output = Vec::with_capacity(stdout.len().saturating_add(stderr.len()));
    output.extend_from_slice(stdout);
    output.extend_from_slice(stderr);
    for path in &case.stdout_contains {
        let expected = fs::read(path)?;
        if !expected.is_empty() && !output.windows(expected.len()).any(|window| window == expected) {
            return Err(format!(
                "stdout does not contain {:?}; {}",
                String::from_utf8_lossy(&expected),
                output_summary(stdout, stderr)
            )
            .into());
        }
    }
    if let Some(path) = &case.stdout_exact {
        let expected = fs::read(path)?;
        if output != expected {
            return Err(format!(
                "combined output differs from {}; {}",
                path.display(),
                output_summary(stdout, stderr)
            )
            .into());
        }
    }
    if let Some(path) = &case.stdout_regex {
        let expression = fs::read_to_string(path)?;
        let pattern = regex::bytes::Regex::new(expression.trim_end_matches(['\r', '\n']))
            .map_err(|error| format!("invalid output expression {}: {error}", path.display()))?;
        if !pattern.is_match(&output) {
            return Err(format!(
                "combined output does not match {}; {}",
                path.display(),
                output_summary(stdout, stderr)
            )
            .into());
        }
    }
    if let Some(path) = &case.stdout_stream_regex {
        let expression = fs::read_to_string(path)?;
        let pattern = regex::bytes::Regex::new(expression.trim_end_matches(['\r', '\n']))
            .map_err(|error| format!("invalid stdout expression {}: {error}", path.display()))?;
        if !pattern.is_match(stdout) {
            return Err(format!(
                "stdout does not match {}; {}",
                path.display(),
                output_summary(stdout, stderr)
            )
            .into());
        }
    }
    let evidence = case.fork_diagnostics.map_or_else(
        || Ok(String::new()),
        |expectation| verify_fork_diagnostics(stderr, expectation.maximum_records),
    )?;
    if case.output_empty && !output.is_empty() {
        return Err(format!("combined output is not empty; {}", output_summary(stdout, stderr)).into());
    }
    Ok(evidence)
}

fn verify_fork_diagnostics(stderr: &[u8], maximum: usize) -> Result<String, Error> {
    const LINUX_EAGAIN: i64 = 11;
    let text = std::str::from_utf8(stderr).map_err(|_| "native diagnostic stderr is not UTF-8")?;
    let native_records = text
        .lines()
        .filter(|line| {
            [
                "[prof]",
                "hl-c:",
                "hl-native:",
                "hl-native-detail:",
                "hl-native-entry:",
                "hl-interp:",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .count();
    if native_records == 0 {
        return Err("native diagnostics emitted no backend-owned counter record".into());
    }
    let records = text
        .lines()
        .filter(|line| line.starts_with("hl-fork-failure:"))
        .collect::<Vec<_>>();
    if records.len() > maximum {
        return Err(format!(
            "native fork diagnostics emitted {} records, limit is {maximum}",
            records.len()
        )
        .into());
    }
    let mut retryable = 0usize;
    let mut stages = std::collections::BTreeMap::<&str, usize>::new();
    for record in &records {
        let (stage, result_errno) = verify_fork_record(record)?;
        *stages.entry(stage).or_default() += 1;
        retryable += usize::from(result_errno == LINUX_EAGAIN);
    }
    let stages = stages
        .into_iter()
        .map(|(stage, count)| format!("{stage}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "native_records={native_records} fork_records={} fork_retryable={retryable} fork_stages={stages}",
        records.len()
    ))
}

const FORK_FIELDS: &[&str] = &[
    "stage",
    "result_errno",
    "ambient_errno",
    "syscall",
    "flags",
    "guest_pc",
    "guest_sp",
    "guest_tid",
    "host_pid",
    "host_ppid",
    "route",
    "worker_pid",
    "sentry_pid",
    "guest_children",
    "worker_threads",
    "ring",
    "host_snapshot_status",
    "host_threads",
    "host_children",
    "children_truncated",
    "local_tasks",
    "pids_total",
    "pids_max",
    "open_fds",
    "nofile_cur",
    "nofile_max",
    "nofile_status",
    "nproc_cur",
    "nproc_max",
    "nproc_status",
    "mem_charged",
    "mem_max",
    "snapshot_stage",
    "ofd_count",
    "ofd_bytes",
    "ofd_capacity",
    "ofd_capacity_bytes",
    "ofd_watermark",
    "reserved_fds",
    "watch_count",
    "watch_bytes",
    "watch_capacity",
    "watch_capacity_bytes",
    "fdvis_count",
    "fdvis_bytes",
    "watch_prepared",
    "private_prepared",
    "fdvis_prepared",
    "seq_prepared",
];

fn verify_fork_record(record: &str) -> Result<(&str, i64), Error> {
    let mut tokens = record.split_ascii_whitespace();
    if tokens.next() != Some("hl-fork-failure:") {
        return Err("native fork diagnostic has an invalid prefix".into());
    }
    let mut stage = None;
    let mut result_errno = None;
    for expected in FORK_FIELDS {
        let token = tokens
            .next()
            .ok_or_else(|| format!("native fork diagnostic lacks {expected}: {record}"))?;
        let (name, value) = token
            .split_once('=')
            .ok_or_else(|| format!("native fork diagnostic has a bare token: {record}"))?;
        if name != *expected || value.is_empty() {
            return Err(format!("native fork diagnostic expected {expected}, found {token}: {record}").into());
        }
        verify_fork_field(name, value, record)?;
        match name {
            "stage" => stage = Some(value),
            "result_errno" => result_errno = Some(value.parse::<i64>().expect("validated result errno")),
            _ => {}
        }
    }
    if let Some(token) = tokens.next() {
        return Err(format!("native fork diagnostic has an unknown trailing token {token}: {record}").into());
    }
    Ok((
        stage.expect("schema contains stage"),
        result_errno.expect("schema contains result errno"),
    ))
}

fn verify_fork_field(name: &str, value: &str, record: &str) -> Result<(), Error> {
    let valid = match name {
        "stage" => matches!(
            value,
            "sentry-clone3-arguments"
                | "sentry-snapshot"
                | "sentry-sync-pipe"
                | "sentry-install"
                | "sentry-sync-publish"
                | "namespace-flags"
                | "thread-spawn"
                | "pids-limit"
                | "share-fs"
                | "snapshot-prepare"
                | "vfork-pipe"
                | "task-prepare"
                | "pid-allocate"
                | "host-fork"
                | "identity-publish"
                | "snapshot-complete"
                | "task-publish"
                | "clone3-size"
                | "clone3-arguments"
        ),
        "route" => matches!(value, "local" | "sentry-worker"),
        "snapshot_stage" => matches!(
            value,
            "none"
                | "private-prepare"
                | "fdvis-prepare"
                | "prepared"
                | "watch-allocation"
                | "watch-prepare"
                | "ofd-allocation"
                | "ofd-prepare"
                | "completed"
                | "complete-failed"
        ),
        "flags" | "guest_pc" | "guest_sp" => {
            let digits = value.strip_prefix("0x").unwrap_or(value);
            !digits.is_empty() && u64::from_str_radix(digits, 16).is_ok()
        }
        "result_errno"
        | "ambient_errno"
        | "guest_tid"
        | "host_pid"
        | "host_ppid"
        | "worker_pid"
        | "sentry_pid"
        | "guest_children"
        | "worker_threads"
        | "ring"
        | "host_snapshot_status"
        | "host_threads"
        | "host_children"
        | "children_truncated"
        | "local_tasks"
        | "open_fds"
        | "nofile_status"
        | "nproc_status" => value.parse::<i64>().is_ok(),
        "watch_prepared" | "private_prepared" | "fdvis_prepared" | "seq_prepared" => {
            matches!(value, "0" | "1")
        }
        _ => value.parse::<u64>().is_ok(),
    };
    if !valid {
        return Err(format!("native fork diagnostic has invalid {name}={value}: {record}").into());
    }
    Ok(())
}

pub(super) fn readiness_logs(rootfs: &std::path::Path, paths: &[String]) -> String {
    paths
        .iter()
        .take(8)
        .map(|path| {
            let relative = std::path::Path::new(path)
                .strip_prefix("/")
                .unwrap_or_else(|_| std::path::Path::new(path));
            if relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return format!("{path}=<invalid path>");
            }
            match fs::read(rootfs.join(relative)) {
                Ok(bytes) => format!("{path}={:?}", String::from_utf8_lossy(excerpt(&bytes))),
                Err(error) => format!("{path}=<unavailable: {error}>"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) async fn wait(
    containers: &hl_container::Containers,
    name: &str,
    timeout: Duration,
) -> Result<ExitStatus, Error> {
    let waiting = containers.wait(name);
    tokio::pin!(waiting);
    let deadline = Instant::now() + timeout;
    loop {
        tokio::select! {
            result = &mut waiting => return Ok(result?),
            () = tokio::time::sleep_until(deadline) => {
                return Err(format!("timed out after {} milliseconds", timeout.as_millis()).into());
            }
            () = tokio::time::sleep(Duration::from_millis(10)) => {
                containers.logs(name).await?.bounded()?;
            }
        }
    }
}

fn output_summary(stdout: &[u8], stderr: &[u8]) -> String {
    format!("stdout={:?}; stderr={:?}", excerpt(stdout), excerpt(stderr))
}

pub(super) fn combine<T>(primary: Result<T, Error>, secondary: Result<(), String>) -> Result<T, Error> {
    match (primary, secondary) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(secondary)) => Err(secondary.into()),
        (Err(primary), Err(secondary)) => Err(format!("{primary}; {secondary}").into()),
    }
}

fn excerpt(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.len().min(DIAGNOSTIC_LIMIT)]
}

pub(super) fn diagnostic(error: &str) -> String {
    let mut result = String::with_capacity(error.len().min(DIAGNOSTIC_LIMIT));
    for character in error.chars() {
        let character = match character {
            '\t' | '\n' | '\r' => ' ',
            value => value,
        };
        if result.len() + character.len_utf8() > DIAGNOSTIC_LIMIT {
            break;
        }
        result.push(character);
    }
    result
}
