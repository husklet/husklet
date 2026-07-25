use super::{queue::TASKS, Error, Options};
use crate::report::{BatchMetadata, BatchReport, Store};
use std::{collections::BTreeMap, env, path::PathBuf};

pub(super) fn finish(options: &Options, run: &str) -> Result<(), Error> {
    let report_base = env::var_os("HL_SCENARIO_REPORT_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("hl-daemon belongs to a workspace")
                .join(".cache/scenario-runs")
        },
        PathBuf::from,
    );
    let store = Store::create(&report_base, run)?;
    store.finish(&BatchReport::new(
        BatchMetadata {
            run_id: run.into(),
            started_unix_ms: 0,
            engine_archive_hash: env::var("HL_ENGINE_ARCHIVE_SHA256")
                .unwrap_or_else(|_| "unknown".into()),
            targets: vec![options.target.name().into()],
            images: BTreeMap::new(),
            categories: TASKS
                .iter()
                .map(|task| task.category.into())
                .chain(crate::workflows::NAMES.into_iter().map(String::from))
                .collect(),
            filters: options
                .case
                .iter()
                .chain(options.category.iter())
                .cloned()
                .collect(),
        },
        store.resume()?.into_values().collect(),
        store.resume_workflows()?.into_values().collect(),
    ))?;
    crate::analyze::generate(&report_base.join(run), None)?;
    Ok(())
}
