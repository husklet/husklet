use crate::{analyze, contract, languages, manifest, provenance, registry, report, scheduler};

pub(super) fn parity() -> Result<(), Box<dyn std::error::Error>> {
    registry::build()
        .verify(include_str!("../golden/scenario.contracts"))
        .map_err(Into::into)
}

pub(super) fn self_test() -> Result<(), Box<dyn std::error::Error>> {
    parity()?;
    contract::test_firewall()?;
    manifest::test_validation()?;
    scheduler::test_requirements().map_err(|error| error.to_string())?;
    scheduler::test_run_lock().map_err(|error| error.to_string())?;
    scheduler::test_workflow_target_cache();
    scheduler::tests::run_ids_survive_process_id_reuse();
    analyze::tests::normalization_removes_volatile_values();
    languages::tests::registry_has_every_stable_id_once();
    scheduler::tests::options_reject_zero_jobs_and_accept_filters();
    crate::contract::test_target_routing();
    crate::fixture::test_platform_aware_cache_resolution()?;
    report::tests::persistence_resume_and_summaries_are_deterministic();
    report::tests::summary_categories_cover_every_recorded_outcome();
    report::tests::legacy_resume_filters_before_case_body();
    report::tests::concurrent_category_writers_preserve_every_result();
    report::tests::architecture_skip_writes_exactly_one_terminal_result();
    report::tests::workflow_evidence_is_append_only_resumable_and_summarized();
    provenance::tests::rejects_patch_artifacts_and_mismatched_sources();
    Ok(())
}
