use super::{Error, diagnostic, output};
use hl_container::{ExitStatus, Logs};

pub(super) fn validate(
    status: ExitStatus,
    expected_exit: i32,
    logs: &Logs,
    expected_stdout: &[u8],
    expected_stderr: &[String],
    profile: Result<(), Error>,
) -> Result<(), Error> {
    if status != ExitStatus::Code(expected_exit) {
        let diagnostic = String::from_utf8_lossy(&logs.stderr)
            .chars()
            .take(4096)
            .collect::<String>();
        let suffix = (!diagnostic.is_empty())
            .then(|| format!("; stderr={diagnostic:?}"))
            .unwrap_or_default();
        return Err(format!("exit {status:?}, expected {expected_exit}{suffix}").into());
    }
    if logs.stdout != expected_stdout {
        return Err(diagnostic::compare("stdout", &logs.stdout, expected_stdout).into());
    }
    if let Some(violation) = output::stderr_violation(expected_stderr, &logs.stderr) {
        return Err(violation.into());
    }
    profile
}
