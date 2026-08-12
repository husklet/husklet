//! Application composition boundary for host process construction.

use std::{ffi::OsStr, io};

/// Owns construction of host processes used by the testing application.
pub(crate) struct HostProcess;

impl HostProcess {
    pub(crate) fn standard(program: impl AsRef<OsStr>) -> std::process::Command {
        std::process::Command::new(program)
    }

    pub(crate) fn exact_process_count(name: &str) -> io::Result<u64> {
        let output = Self::standard("pgrep").args(["-cx", name]).output()?;
        decode_process_count(output.status.code(), &output.stdout, &output.stderr)
    }
}

fn decode_process_count(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> io::Result<u64> {
    let count = std::str::from_utf8(stdout)
        .map_err(io::Error::other)?
        .trim()
        .parse::<u64>()
        .map_err(io::Error::other)?;
    match (code, count) {
        (Some(0), count) => Ok(count),
        (Some(1), 0) => Ok(0),
        _ => Err(io::Error::other(format!(
            "pgrep failed with status {}: {}",
            code.map_or_else(|| "signal".to_owned(), |value| value.to_string()),
            String::from_utf8_lossy(stderr).trim()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::decode_process_count;

    #[test]
    fn process_count_requires_a_valid_success_or_no_match_result() {
        assert_eq!(decode_process_count(Some(0), b"7\n", b"").unwrap(), 7);
        assert_eq!(decode_process_count(Some(1), b"0\n", b"").unwrap(), 0);
        assert!(decode_process_count(Some(2), b"0\n", b"bad pattern").is_err());
        assert!(decode_process_count(Some(0), b"not-a-count\n", b"").is_err());
        assert!(decode_process_count(None, b"0\n", b"").is_err());
    }
}
