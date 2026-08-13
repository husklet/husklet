//! Controlled external/retained/integrated C-engine acceptance measurements.

mod definition;
mod evidence;
mod schedule;
mod stage;
mod verdict;

use crate::suite::Error;
use clap::Args;
use definition::Campaign;
use evidence::{Ledger, Measurement};
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct Options {
    /// Strict campaign definition beneath the repository workspace.
    #[arg(long)]
    config: PathBuf,
    /// New result directory beneath the repository workspace.
    #[arg(long)]
    results: PathBuf,
    /// Continue the exact campaign recorded in an interrupted result directory.
    #[arg(long)]
    resume: bool,
    /// Maximum accepted integrated/baseline ratio.
    #[arg(long, default_value_t = 1.10)]
    limit: f64,
    /// Minimum free space required before measurement.
    #[arg(long, default_value_t = 30.0)]
    minimum_free_gib: f64,
    /// Consecutive quiet seconds required before taking the box lock.
    #[arg(long, default_value_t = 120)]
    quiet_seconds: u64,
    /// Maximum wait for quiet and locks.
    #[arg(long, default_value_t = 900)]
    lock_timeout: u64,
    /// Maximum accepted one-minute host load.
    #[arg(long, default_value_t = 1.0)]
    max_load: f64,
}

#[derive(Args)]
pub(crate) struct HashOptions {
    /// File or directory to identify using the campaign's hashing protocol.
    path: PathBuf,
}

pub(crate) use stage::Options as StageOptions;

pub(crate) fn hash(options: HashOptions) -> Result<(), Error> {
    println!("{}", definition::artifact_identity(&options.path)?);
    Ok(())
}

pub(crate) fn stage(options: StageOptions) -> Result<(), Error> {
    stage::run(options)
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    let workspace = crate::runtime::workspace()?;
    let config_path = workspace.join(&options.config);
    let campaign = Campaign::load(&config_path)?;
    campaign.verify_artifacts()?;
    let result_path = workspace.join(&options.results);
    let mut ledger = Ledger::open(&result_path, &campaign, options.resume)?;
    ledger.require_space(options.minimum_free_gib)?;
    let _measurement = Measurement::acquire(options.quiet_seconds, options.lock_timeout, options.max_load)?;

    // A resumed process has cold process/cache state, so warmups deliberately run again.
    for step in schedule::warmups(&campaign) {
        evidence::sample(&campaign, &step)?;
    }
    for step in schedule::measurements(&campaign) {
        if ledger.contains(&step.key()) {
            continue;
        }
        let row = evidence::measure(&campaign, &step)?;
        ledger.append(&row)?;
    }
    // A writable guest or replaced binary invalidates the whole campaign, even if its output
    // happened to remain stable. Re-hash after the last sample as well as before the first.
    campaign.verify_artifacts()?;
    let rows = ledger.complete()?;
    let report = verdict::Report::evaluate(&campaign, &rows, options.limit)?;
    ledger.publish(&report)?;
    print!("{}", report.text);
    println!(
        "VERDICT\t{}\tlimit={:.3}\tidentity={}",
        report.verdict,
        options.limit,
        campaign.identity()?
    );
    if report.verdict == "PASS" {
        Ok(())
    } else {
        Err("benchmark acceptance limit exceeded".into())
    }
}
