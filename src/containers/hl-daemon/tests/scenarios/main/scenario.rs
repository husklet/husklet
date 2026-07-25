use crate::{
    api,
    api::support::{containers_for, unpack},
    coherence, copy, databases, distros, execcmd, filesystem, imagescmd, languages, lifecycle,
    netcontainer, network, observe, permissions, process, registry, report, runflags, runner,
    terminal, utilities, web, weird, workflows,
};
use std::{env, path::PathBuf};
use tempfile::TempDir;

pub(super) const SCENARIOS: [&str; 32] = [
    "runtime-alpine",
    "copy",
    "buildcmd",
    "databases",
    "distros",
    "execcmd",
    "imagescmd",
    "languages",
    "netcontainer",
    "network-contracts",
    "observe",
    "filesystem",
    "lifecycle",
    "permissions",
    "runflags",
    "terminal",
    "toolchains",
    "utilities",
    "descendant-cleanup",
    "volume-contracts",
    "web",
    "weird",
    "headless-lifecycle",
    "persistence-restart",
    "concurrent-clients",
    "removal-wait-race",
    "http-errors",
    "malformed-image-archive",
    "image-archive-create",
    "container-copy",
    "server-restart-persistence",
    "server-process",
];

pub(super) async fn workflow() -> Result<(), Box<dyn std::error::Error>> {
    let name = env::args().nth(2).ok_or("workflow name is required")?;
    let work = TempDir::new()?;
    workflows::run(&name, &containers_for(work.path()).await?).await
}

pub(super) async fn quick() -> Result<(), Box<dyn std::error::Error>> {
    let mut results = Vec::with_capacity(SCENARIOS.len());
    record("runtime-alpine", runtime().await, &mut results);
    record(
        "headless-lifecycle",
        api::test_headless_lifecycle::run().await,
        &mut results,
    );
    record(
        "persistence-restart",
        api::test_persistence_restart::run().await,
        &mut results,
    );
    record(
        "concurrent-clients",
        api::test_concurrent_clients::run().await,
        &mut results,
    );
    record(
        "removal-wait-race",
        api::test_removal_wait_race::run().await,
        &mut results,
    );
    record(
        "http-errors",
        api::test_http_errors::run().await,
        &mut results,
    );
    record(
        "malformed-image-archive",
        api::test_malformed_image_archive::run().await,
        &mut results,
    );
    record(
        "image-archive-create",
        api::test_image_archive::run().await,
        &mut results,
    );
    record(
        "container-copy",
        api::test_container_copy::run().await,
        &mut results,
    );
    record(
        "server-restart-persistence",
        api::test_server_restart_persistence::run().await,
        &mut results,
    );
    record(
        "server-process",
        api::test_server_process::run().await,
        &mut results,
    );

    let passed = results.iter().filter(|(_, result)| result.is_ok()).count();
    println!(
        "{{\"suite\":\"quick\",\"total\":{},\"passed\":{},\"failed\":{},\"results\":[{}]}}",
        results.len(),
        passed,
        results.len() - passed,
        results
            .iter()
            .map(|(name, result)| match result {
                Ok(()) => format!("{{\"name\":\"{name}\",\"status\":\"passed\"}}"),
                Err(error) => format!(
                    "{{\"name\":\"{name}\",\"status\":\"failed\",\"error\":\"{}\"}}",
                    json_escape(error)
                ),
            })
            .collect::<Vec<_>>()
            .join(",")
    );
    if passed == results.len() {
        Ok(())
    } else {
        Err(format!(
            "{} of {} quick scenarios failed",
            results.len() - passed,
            results.len()
        )
        .into())
    }
}

pub(super) async fn runtime() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    let containers = containers_for(work.path()).await?;
    execcmd::run(&containers, &rootfs).await?;
    imagescmd::run().await?;
    netcontainer::run(&containers, &rootfs).await?;
    observe::run(&containers, &rootfs, work.path()).await?;
    filesystem::run(&containers).await?;
    api::test_named_volume::run(work.path(), &rootfs).await?;
    api::test_headless_runtime::run(&containers, &rootfs).await?;
    coherence::run(&containers, &rootfs).await?;
    api::test_resources::run(&containers, &rootfs).await?;
    process::run(&containers, &rootfs).await?;
    lifecycle::run(&containers, &rootfs).await?;
    permissions::run(&containers).await?;
    utilities::run(&containers).await?;
    api::test_network_bridge::run(&containers, &rootfs).await?;
    api::test_port_publishing::run(&containers, &rootfs).await?;
    api::test_daemon_runtime::run(containers, &rootfs, work.path()).await?;
    println!("PASS runtime-alpine");
    Ok(())
}

pub(super) async fn filesystem() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    filesystem::run(&containers_for(work.path()).await?).await
}

pub(super) async fn copy() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    copy::run(&containers_for(work.path()).await?, &rootfs).await
}

pub(super) async fn build() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = containers_for(work.path()).await?;
    let scenarios = registry::build::group().scenarios;
    let mut reports = report::ScenarioBatch::new("buildcmd")?;
    let mut attempts = Vec::new();
    for scenario in &scenarios {
        if let Some(attempt) = reports.begin(scenario)? {
            attempts.push((scenario, attempt));
        }
    }
    if attempts.is_empty() {
        return Ok(());
    }
    let result = workflows::run("docker-build", &containers).await;
    for (scenario, attempt) in attempts {
        reports.complete(scenario, attempt, &result)?;
    }
    reports.finish(Vec::new())?;
    result
}

pub(super) async fn databases() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = containers_for(work.path()).await?;
    runner::Runner::arm64(&containers)
        .run(databases::group())
        .await
}

pub(super) async fn database_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    databases::cleanup_probe(&containers_for(work.path()).await?, &rootfs).await
}

pub(super) async fn exec() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    execcmd::run(&containers_for(work.path()).await?, &rootfs).await
}

pub(super) async fn lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    lifecycle::run(&containers_for(work.path()).await?, &rootfs).await
}

pub(super) async fn languages() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    languages::run(work.path()).await
}

pub(super) async fn toolchains() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = containers_for(work.path()).await?;
    runner::Runner::arm64(&containers)
        .run(registry::toolchains::group())
        .await
}

pub(super) async fn distros() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    distros::run(&containers_for(work.path()).await?).await
}

pub(super) async fn weird() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    weird::run(&containers_for(work.path()).await?).await
}

pub(super) async fn web() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    web::run(&containers_for(work.path()).await?).await
}

pub(super) async fn runflags() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    runflags::run(&containers_for(work.path()).await?, &rootfs, work.path()).await
}

pub(super) async fn permissions() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    permissions::run(&containers_for(work.path()).await?).await
}

pub(super) async fn process() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    process::run(&containers_for(work.path()).await?, &rootfs).await
}

pub(super) async fn terminal() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    terminal::run(&containers_for(work.path()).await?).await
}

pub(super) async fn observe() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    observe::run(&containers_for(work.path()).await?, &rootfs, work.path()).await
}

pub(super) async fn netcontainer() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let rootfs = alpine(work.path()).await?;
    netcontainer::run(&containers_for(work.path()).await?, &rootfs).await
}

pub(super) async fn network() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    network::run(&containers_for(work.path()).await?).await
}

pub(super) async fn utilities() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    utilities::run(&containers_for(work.path()).await?).await
}

async fn alpine(work: &std::path::Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let rootfs = work.join("rootfs");
    let archive = env::var_os("HL_ALPINE_ARCHIVE")
        .map(PathBuf::from)
        .ok_or("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs")?;
    unpack(archive, rootfs.clone()).await?;
    Ok(rootfs)
}

fn record(
    name: &'static str,
    result: Result<(), Box<dyn std::error::Error>>,
    results: &mut Vec<(&'static str, Result<(), String>)>,
) {
    if let Err(error) = &result {
        eprintln!("FAIL {name}: {error}");
    }
    results.push((name, result.map_err(|error| error.to_string())));
}

fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            value if value.is_control() => '\u{fffd}'.to_string().chars().collect(),
            value => vec![value],
        })
        .collect()
}
