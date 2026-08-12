use crate::engine::{EngineExit, ExitKind};
use crate::launch_plan::RuntimeLaunchPlan;
use std::io::Write;

pub(super) fn requested(plan: &RuntimeLaunchPlan) -> bool {
    plan.options.get("HL_C_EXECUTION_ATTESTATION").is_some()
}

/// Emits from the host-owned supervisor stderr only after the caller has
/// matched the C worker's framed exit with its OS process status.
pub(super) fn report_exit(exit: &mut EngineExit, enabled: bool) {
    let translations = if enabled { exit.detail } else { 0 };
    if enabled && exit.kind == ExitKind::Code {
        exit.detail = 0;
    }
    report(std::io::stderr().lock(), enabled, translations);
}

fn report(mut output: impl Write, enabled: bool, translations: u64) {
    if enabled {
        let _ = writeln!(output, "hl-c: runs=1 builds={translations}");
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{EngineExit, ExitKind};

    #[test]
    fn completion_record_is_explicit_and_non_vacuous() {
        let mut enabled = Vec::new();
        super::report(&mut enabled, true, 37);
        assert_eq!(enabled, b"hl-c: runs=1 builds=37\n");
        let mut disabled = Vec::new();
        super::report(&mut disabled, false, 37);
        assert!(disabled.is_empty());
    }

    #[test]
    fn private_translation_payload_never_changes_the_public_exit() {
        let mut exit = EngineExit {
            kind: ExitKind::Code,
            guest_status: 0,
            detail: 37,
            fault: None,
        };
        super::report_exit(&mut exit, true);
        assert_eq!(exit.detail, 0);
    }
}
