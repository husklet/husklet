use crate::engine::EngineError;

pub(super) fn c_option(name: &str) -> bool {
    !matches!(
        name,
        "HL_EXECUTION_BACKEND"
            | "HL_C_EXECUTION_ATTESTATION"
            | "HL_A64_DIRTY_OVERFLOW_CONTINUE"
            | "HL_A64_DIRTY_OVERFLOW_EXIT"
            | "HL_A64_NO_WRITE_COMMIT"
            | "HL_A64_NO_WRITE_RESERVE"
            | "HL_A64_RUNTIME_WRITE_RESERVE"
            | "HL_NATIVE_ADMISSION_CACHE"
            | "HL_NATIVE_DIAGNOSTICS"
            | "HL_NATIVE_DIRECT_HOLD_RUNS"
            | "HL_NATIVE_DIRECT_STICKY"
            | "HL_NATIVE_DIRECT_STICKY_LIMIT"
            | "HL_NATIVE_DIRECT_STICKY_PERMANENT"
            | "HL_NATIVE_EXECUTION"
            | "HL_NATIVE_SPLIT_MODE_EXECUTORS"
            | "HL_C_NO_RUNTIME_EXIT"
            | "HL_C_NO_RUNTIME_IDENTITY"
            | "HL_SECCOMP_BASELINE"
    )
}

fn c_volume_path(value: &str) -> String {
    value.bytes().fold(String::new(), |mut output, byte| {
        if matches!(byte, b'%' | b':' | b',') {
            use std::fmt::Write as _;
            write!(output, "%{byte:02X}").expect("writing to a String cannot fail");
        } else {
            output.push(char::from(byte));
        }
        output
    })
}

pub(super) fn c_file_volumes(value: &str) -> Result<Vec<String>, EngineError> {
    value
        .lines()
        .map(|record| {
            let (source, guest) = record.split_once('\t').ok_or(EngineError::LaunchFailed)?;
            let (access, source) = source.split_once(':').ok_or(EngineError::LaunchFailed)?;
            if !matches!(access, "ro" | "rw") || source.is_empty() || !guest.starts_with('/') {
                return Err(EngineError::LaunchFailed);
            }
            Ok(format!(
                "v2:{access}:{}:{}",
                c_volume_path(guest),
                c_volume_path(source)
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::c_option;

    #[test]
    fn supervisor_attestation_never_reaches_the_c_option_store() {
        assert!(!c_option("HL_C_EXECUTION_ATTESTATION"));
        assert!(c_option("HL_C_DIAGNOSTICS"));
    }
}
