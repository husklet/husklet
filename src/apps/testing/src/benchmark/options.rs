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
    /// Independent samples per measured row; 5 is available for noisy hosts.
    #[arg(long, default_value_t = 3, value_parser = parse_samples)]
    pub samples_per_row: u32,
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

fn parse_samples(value: &str) -> Result<u32, String> {
    match value.parse::<u32>() {
        Ok(value @ (3 | 5)) => Ok(value),
        _ => Err("samples per row must be 3 or 5".into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn samples_per_row_is_bounded_to_supported_estimators() {
        assert_eq!(super::parse_samples("3"), Ok(3));
        assert_eq!(super::parse_samples("5"), Ok(5));
        for invalid in ["0", "4", "6", "five"] {
            assert!(super::parse_samples(invalid).is_err());
        }
    }
}
