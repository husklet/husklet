use super::definition::{Scenario, ScenarioAction};
use crate::{
    runtime,
    suite::{Error, Target},
};
use clap::Args;
use std::collections::BTreeSet;

const LEGACY_WORKFLOWS: [&str; 8] = [
    "docker-build",
    "docker-net",
    "docker-full",
    "compose",
    "compose-multinet",
    "pty-conformance",
    "realsw",
    "smoke-realimage",
];

#[derive(Args)]
pub(crate) struct ProvenanceOptions {
    /// Print one tab-separated row per YAML case.
    #[arg(long)]
    details: bool,
}

#[derive(Args)]
pub(crate) struct CachePreflightOptions {
    /// Guest architecture whose exact cache leaf is checked.
    #[arg(value_enum)]
    target: Target,
}

pub(super) fn inventory(scenarios: Vec<Scenario>) -> Result<(), Error> {
    let cases = scenarios.iter().map(|scenario| scenario.cases.len()).sum::<usize>();
    let targets = scenarios
        .iter()
        .flat_map(|scenario| &scenario.cases)
        .map(|case| case.targets.len())
        .sum::<usize>();
    let images = image_references(&scenarios).len();
    println!(
        "{{\"scenario_definitions\":{},\"scenario_cases\":{cases},\"case_target_pairs\":{targets},\"image_references\":{images},\"legacy_workflows\":{}}}",
        scenarios.len(),
        LEGACY_WORKFLOWS.len()
    );
    Ok(())
}

pub(super) fn provenance(scenarios: Vec<Scenario>, options: ProvenanceOptions) -> Result<(), Error> {
    let mut ids = BTreeSet::new();
    let mut opaque = Vec::new();
    for scenario in &scenarios {
        for case in &scenario.cases {
            if !ids.insert(case.id.as_str()) {
                return Err(format!("duplicate scenario ID {}", case.id).into());
            }
            if case
                .actions
                .iter()
                .any(|action| matches!(action, ScenarioAction::Host(_)))
            {
                opaque.push(case.id.as_str());
            }
            if options.details {
                println!(
                    "{}\t{}\t{}\t{}",
                    scenario.definition.display(),
                    case.id,
                    case.targets
                        .iter()
                        .map(|target| target.name())
                        .collect::<Vec<_>>()
                        .join(","),
                    case.image
                );
            }
        }
    }
    println!(
        "scenario provenance: definitions={}; cases={}; images={}; opaque_actions={}",
        scenarios.len(),
        ids.len(),
        image_references(&scenarios).len(),
        opaque.len()
    );
    if opaque.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} cases retain opaque host/api actions; first: {}",
            opaque.len(),
            opaque.into_iter().take(10).collect::<Vec<_>>().join(", ")
        )
        .into())
    }
}

pub(super) fn workflows() {
    for name in LEGACY_WORKFLOWS {
        println!("{name}");
    }
}

pub(super) fn cache_preflight(scenarios: Vec<Scenario>, options: CachePreflightOptions) -> Result<(), Error> {
    let references = scenarios
        .iter()
        .flat_map(|scenario| &scenario.cases)
        .filter(|case| case.supports(options.target))
        .map(|case| case.image.as_str())
        .collect::<BTreeSet<_>>();
    let mut missing = Vec::new();
    for reference in &references {
        if !runtime::preflight_image(reference, options.target)? {
            missing.push(*reference);
        }
    }
    if missing.is_empty() {
        println!(
            "offline OCI preflight passed: {} references for {}",
            references.len(),
            options.target.name()
        );
        Ok(())
    } else {
        Err(format!(
            "offline OCI preflight failed for {}: {} of {} references absent: {}",
            options.target.name(),
            missing.len(),
            references.len(),
            missing.join(", ")
        )
        .into())
    }
}

fn image_references(scenarios: &[Scenario]) -> BTreeSet<&str> {
    scenarios
        .iter()
        .flat_map(|scenario| &scenario.cases)
        .map(|case| case.image.as_str())
        .collect()
}
