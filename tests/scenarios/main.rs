//! Daemon integration and Linux guest compatibility runner.

mod groups;
mod harness;
mod provenance;
mod registry;
mod support;
#[path = "workflows/mod.rs"]
mod workflows;

#[path = "main/cache.rs"]
mod cache_command;
#[path = "main/report.rs"]
mod report_command;
#[path = "main/scenario.rs"]
mod scenario;
#[path = "main/validation.rs"]
mod validation;

// Re-export the grouped testing-engine modules at the crate root so existing
// `crate::report::…` / `super::runner::…` etc. paths keep resolving unchanged.
#[allow(
    dead_code,
    reason = "batch runner integration is owned by the scheduler and runner"
)]
pub(crate) use harness::{analyze, contract, fixture, manifest, report, runner, scheduler};

// Re-export the scenario category groups at the crate root so existing
// `crate::<category>::group()` / `crate::languages::tests::…` paths keep resolving.
#[allow(
    unused_imports,
    reason = "categories are referenced through the registry catalog and provenance auditor"
)]
pub(crate) use groups::{
    coherence, copy, databases, distros, execcmd, languages, lifecycle, netcontainer, network,
    permissions, process, runflags, terminal, utilities, volume, weird,
};

use contract::Target;
use std::{collections::BTreeSet, env};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("daemon scenarios: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    match env::args().nth(1).as_deref().unwrap_or("all") {
        "all" => {
            validation::self_test()?;
            let options = scheduler::Options::parse(env::args().skip(2))
                .map_err(|error| error.to_string())?;
            scheduler::run(options)
                .await
                .map_err(|error| error.to_string())?;
        }
        "list" => {
            for name in scenario::SCENARIOS {
                println!("{name}\tquick");
            }
        }
        "workflows" => {
            for name in workflows::NAMES {
                println!("{name}");
            }
        }
        "workflow" => scenario::workflow().await?,
        "parity" => validation::parity()?,
        "provenance" => provenance::audit(false)?,
        "provenance-strict" => provenance::audit(true)?,
        "contracts" => print!("{}", registry::build().snapshot()),
        "inventory" => report_command::inventory(),
        "report-partial" => report_command::partial()?,
        "report-invalidate" => report_command::invalidate()?,
        "cache-quarantine" => cache_command::quarantine()?,
        "cache-preflight" => cache_preflight()?,
        "contract-test" => {
            contract::test_firewall()?;
            weird::test_expected_failures()?;
        }
        "manifest-test" => manifest::test_validation()?,
        "scheduler-test" => {
            scheduler::test_requirements().map_err(|error| error.to_string())?;
            scheduler::test_run_lock().map_err(|error| error.to_string())?;
            scheduler::tests::run_ids_survive_process_id_reuse();
            scheduler::tests::timeout_reaps_owned_process_group().await;
            runner::test_resources()
                .await
                .map_err(|error| error.to_string())?;
        }
        "service-diagnostics-test" => runner::test_service_diagnostics()?,
        "self-test" => validation::self_test()?,
        "quick" => scenario::quick().await?,
        "runtime-alpine" => scenario::runtime().await?,
        "copy" => scenario::copy().await?,
        "buildcmd" => scenario::build().await?,
        "databases" => scenario::databases().await?,
        "database-cleanup" => scenario::database_cleanup().await?,
        "distros" => scenario::distros().await?,
        "execcmd" => scenario::exec().await?,
        "languages" => scenario::languages().await?,
        "netcontainer" => scenario::netcontainer().await?,
        "network-contracts" => scenario::network().await?,
        "lifecycle" => scenario::lifecycle().await?,
        "permissions" => scenario::permissions().await?,
        "process" => scenario::process().await?,
        "runflags" => scenario::runflags().await?,
        "terminal" => scenario::terminal().await?,
        "toolchains" => scenario::toolchains().await?,
        "utilities" => scenario::utilities().await?,
        "volume-contracts" => volume::run().await?,
        "weird" => scenario::weird().await?,
        other => return Err(format!("unknown scenario {other:?}").into()),
    }
    Ok(())
}

fn cache_preflight() -> Result<(), Box<dyn std::error::Error>> {
    let target = env::args()
        .nth(2)
        .ok_or("cache-preflight requires arm64 or amd64")?;
    let target = match target.as_str() {
        "arm64" => Target::Arm64,
        "amd64" => Target::Amd64,
        other => return Err(format!("unsupported cache-preflight target {other:?}").into()),
    };
    let references = registry::build()
        .scenarios()
        .filter(|scenario| scenario.targets.contains(&target))
        .map(|scenario| scenario.image)
        .collect::<BTreeSet<_>>();
    let mut checked = 0;
    let mut missing = Vec::new();
    for reference in references {
        checked += 1;
        if !fixture::preflight(reference, &target.platform())? {
            missing.push(reference);
        }
    }
    if missing.is_empty() {
        println!(
            "offline OCI preflight passed: {checked} references for {}",
            target.name()
        );
        Ok(())
    } else {
        Err(format!(
            "offline OCI preflight failed for {}: {} of {checked} references absent: {}",
            target.name(),
            missing.len(),
            missing.join(", ")
        )
        .into())
    }
}
