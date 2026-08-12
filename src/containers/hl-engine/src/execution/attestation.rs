use crate::launch_plan::RuntimeLaunchPlan;
use std::io::Write;

pub(super) fn requested(plan: &RuntimeLaunchPlan) -> bool {
    plan.options.get("HL_C_EXECUTION_ATTESTATION").is_some()
}

/// Emits from the host-owned supervisor stderr only after the caller has
/// matched the C worker's framed exit with its OS process status.
pub(super) fn report_completed(enabled: bool) {
    report(std::io::stderr().lock(), enabled);
}

fn report(mut output: impl Write, enabled: bool) {
    if enabled {
        let _ = writeln!(output, "hl-c: runs=1");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn completion_record_is_explicit_and_non_vacuous() {
        let mut enabled = Vec::new();
        super::report(&mut enabled, true);
        assert_eq!(enabled, b"hl-c: runs=1\n");
        let mut disabled = Vec::new();
        super::report(&mut disabled, false);
        assert!(disabled.is_empty());
    }
}
