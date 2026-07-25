use crate::{analyze, registry, report, workflows};
use std::{env, path::PathBuf};

pub(super) fn inventory() {
    let registry = registry::build();
    let scenarios = registry.scenarios().count();
    let images = registry
        .scenarios()
        .map(|scenario| scenario.image)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    println!(
        "{}",
        serde_json::json!({
            "scenario_contracts": scenarios,
            "workflows": workflows::NAMES.len(),
            "all_runtime_cases": scenarios + workflows::NAMES.len(),
            "image_references": images,
            "self_checks": 10,
        })
    );
}

pub(super) fn partial() -> Result<(), Box<dyn std::error::Error>> {
    let base = base();
    let run = env::var("HL_SCENARIO_RUN_ID")?;
    let compare = env::var("HL_SCENARIO_COMPARE_RUN_ID")
        .ok()
        .map(|id| base.join(id));
    let report = analyze::generate(&base.join(run), compare.as_deref())?;
    println!(
        "partial report: {} attempts; {} completed",
        report.attempts, report.completed
    );
    Ok(())
}

pub(super) fn invalidate() -> Result<(), Box<dyn std::error::Error>> {
    let base = base();
    let run = env::var("HL_SCENARIO_RUN_ID")?;
    let archive = env::var("HL_ENGINE_ARCHIVE_SHA256")?;
    let scenarios = env::var("HL_SCENARIO_INVALIDATE")?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let reason = env::var("HL_SCENARIO_INVALIDATE_REASON")?;
    let store = report::Store::create(&base, &run)?;
    let count = store.invalidate(&scenarios, &archive, &reason)?;
    if count != scenarios.len() {
        return Err(format!(
            "corrected {count} exact keys; requested {}",
            scenarios.len()
        )
        .into());
    }
    println!("appended {count} infrastructure corrections");
    Ok(())
}

fn base() -> PathBuf {
    env::var_os("HL_SCENARIO_REPORT_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("hl-daemon belongs to a workspace")
                .join(".cache/scenario-runs")
        },
        PathBuf::from,
    )
}
