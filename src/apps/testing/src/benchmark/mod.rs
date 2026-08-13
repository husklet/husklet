//! Controlled external/retained/integrated C-engine acceptance measurements.

mod calibration;
mod definition;
mod evidence;
mod ledger;
mod options;
mod schedule;
mod stage;
mod verdict;

use crate::suite::Error;
use clap::Args;
use definition::Campaign;
use evidence::Measurement;
use ledger::Ledger;
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Args)]
pub(crate) struct Options {
    #[command(flatten)]
    measurement: options::MeasurementOptions,
    /// Maximum accepted integrated/baseline ratio.
    #[arg(long, default_value_t = 1.10)]
    limit: f64,
}

#[derive(Args)]
pub(crate) struct HashOptions {
    /// File or directory to identify using the campaign's hashing protocol.
    path: PathBuf,
}

pub(crate) use calibration::Options as CalibrationOptions;
pub(crate) use stage::Options as StageOptions;

pub(crate) fn hash(options: HashOptions) -> Result<(), Error> {
    println!("{}", definition::artifact_identity(&options.path)?);
    Ok(())
}

pub(crate) fn stage(options: StageOptions) -> Result<(), Error> {
    stage::run(options)
}

pub(crate) fn calibrate(options: CalibrationOptions) -> Result<(), Error> {
    calibration::run(options)
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    let workspace = crate::runtime::workspace()?;
    let measurement = options.measurement;
    let config_path = workspace.join(&measurement.config);
    let campaign = Campaign::load(&config_path)?;
    campaign.verify_artifacts()?;
    let result_path = workspace.join(&measurement.results);
    let mut ledger = Ledger::open(&result_path, &campaign, measurement.resume)?;
    ledger.require_space(measurement.minimum_free_gib)?;
    let _measurement = Measurement::acquire(
        measurement.quiet_seconds,
        measurement.lock_timeout,
        measurement.max_load,
    )?;

    let measurements = schedule::measurements(&campaign);
    let mut warmed = BTreeSet::new();
    for pair in measurements.chunks_exact(2) {
        let [first, second] = pair else {
            unreachable!("benchmark schedule is made of pairs")
        };
        let present = [ledger.contains(&first.key()), ledger.contains(&second.key())];
        for step in schedule::warmups_for_first_missing(&mut warmed, pair, present == [true, true]) {
            evidence::sample(&campaign, &step)?;
        }
        if present == [true, true] {
            continue;
        }
        if present != [false, false] {
            return Err("benchmark ledger contains an incomplete measurement pair".into());
        }
        ledger.append(&evidence::measure(&campaign, first)?)?;
        ledger.append(&evidence::measure(&campaign, second)?)?;
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
