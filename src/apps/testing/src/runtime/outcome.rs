use super::{Error, diagnostic, output};
use hl_container::{ExitStatus, Logs};

pub(super) fn validate(
    status: ExitStatus,
    expected_exit: i32,
    expected_signal: Option<u8>,
    logs: &Logs,
    expected_stdout: &[u8],
    expected_stderr: &[String],
    profile: Result<(), Error>,
) -> Result<(), Error> {
    let expected = expected_signal.map_or(ExitStatus::Code(expected_exit), |signal| {
        ExitStatus::Signal(i32::from(signal))
    });
    if status != expected {
        let diagnostic = String::from_utf8_lossy(&logs.stderr)
            .chars()
            .take(4096)
            .collect::<String>();
        let suffix = (!diagnostic.is_empty())
            .then(|| format!("; stderr={diagnostic:?}"))
            .unwrap_or_default();
        return Err(format!("exit {status:?}, expected {expected:?}{suffix}").into());
    }
    if logs.stdout != expected_stdout {
        return Err(diagnostic::compare("stdout", &logs.stdout, expected_stdout).into());
    }
    if let Some(violation) = output::stderr_violation(expected_stderr, &logs.stderr) {
        return Err(violation.into());
    }
    profile
}
