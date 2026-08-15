//! Corpus inventory reporting without executing guest cases.

use super::{Options, Schedule, apps, profile, validate_case_ids};
use crate::suite::{Error, Target};
use std::path::PathBuf;

pub(crate) fn run() -> Result<(), Error> {
    let options = Options {
        app: None,
        selection: crate::suite::Selection::all(),
        results: PathBuf::from("target/testing/runtime/inventory-unused.tsv"),
        baseline: None,
        engine_profile: profile::Requested::Release,
        work_root: None,
    };
    let apps = apps(&options)?;
    validate_case_ids(&apps)?;
    let app_count = apps.len();
    let workloads = apps.iter().map(|app| app.cases.len()).sum::<usize>();
    let planned = Schedule::plan(apps, &options);
    let mut active = [0_usize; 2];
    let mut inactive = [0_usize; 2];
    for work in planned.work {
        active[target_index(work.target)] += 1;
    }
    for row in planned.skipped {
        inactive[target_index(row.attempt.key.target)] += 1;
    }
    println!("runtime inventory: apps={app_count} workloads={workloads}");
    println!("runtime inventory: arm64 active={} NOT_RUN={}", active[0], inactive[0]);
    println!("runtime inventory: amd64 active={} NOT_RUN={}", active[1], inactive[1]);
    println!(
        "runtime inventory: rows={} active={} NOT_RUN={}",
        active.iter().sum::<usize>() + inactive.iter().sum::<usize>(),
        active.iter().sum::<usize>(),
        inactive.iter().sum::<usize>()
    );
    Ok(())
}

const fn target_index(target: Target) -> usize {
    match target {
        Target::Arm64 => 0,
        Target::Amd64 => 1,
    }
}
