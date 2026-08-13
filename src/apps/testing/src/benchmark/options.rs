use clap::Args;
use std::path::PathBuf;

/// Host and ledger controls shared by benchmark measurement modes.
#[derive(Args)]
pub(super) struct MeasurementOptions {
    /// Strict campaign definition beneath the repository workspace.
    #[arg(long)]
    pub config: PathBuf,
    /// New result directory beneath the repository workspace.
    #[arg(long)]
    pub results: PathBuf,
    /// Continue the exact campaign recorded in an interrupted result directory.
    #[arg(long)]
    pub resume: bool,
    /// Minimum free space required before measurement.
    #[arg(long, default_value_t = 30.0)]
    pub minimum_free_gib: f64,
    /// Consecutive quiet seconds required before taking the box lock.
    #[arg(long, default_value_t = 120)]
    pub quiet_seconds: u64,
    /// Maximum wait for quiet and locks.
    #[arg(long, default_value_t = 900)]
    pub lock_timeout: u64,
    /// Maximum accepted one-minute host load.
    #[arg(long, default_value_t = 1.0)]
    pub max_load: f64,
}
