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

pub(super) fn verify(case: &Sample, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> Result<(), Error> {
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

pub(super) fn combine(primary: Result<(), Error>, secondary: Result<(), String>) -> Result<(), Error> {
    match (primary, secondary) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(secondary)) => Err(secondary.into()),
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
