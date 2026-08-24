//! Command-line selection for a runtime compatibility sweep.

use super::profile;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named runtime application.
    pub(super) app: Option<String>,
    #[command(flatten)]
    pub(super) selection: crate::suite::Selection,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/runtime/results.tsv", value_parser = crate::suite::parse::results)]
    pub(super) results: PathBuf,
    /// Diff the sweep against a recorded corpus mark instead of against "everything passes".
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "tests/runtime/baseline.tsv",
        value_parser = crate::suite::parse::results,
    )]
    pub(super) baseline: Option<PathBuf>,
    /// Engine build profile this sweep is measuring; must match how the runner was built.
    #[arg(long, value_enum, env = "HL_COMPAT_ENGINE_PROFILE", default_value_t = profile::Requested::Release)]
    pub(super) engine_profile: profile::Requested,
    /// Absolute host-local directory for mutable corpus images, builds, workers, state, and failures.
    #[arg(long, env = "HL_RUNTIME_WORK_ROOT", value_name = "ABSOLUTE_PATH")]
    pub(super) work_root: Option<PathBuf>,
    /// Explicitly execute an exactly selected `!broken` case this many times per ISA without changing corpus policy.
    #[arg(long, value_name = "REPETITIONS", value_parser = clap::value_parser!(u16).range(1..=500))]
    pub(super) broken_soak: Option<u16>,
}
