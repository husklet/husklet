use crate::{
    coherence, copy, databases, distros, execcmd, languages, lifecycle, netcontainer, network,
    permissions, process, registry, report, runflags, runner, terminal, utilities, web, weird,
    workflows,
};
use crate::support::{containers_for, unpack};
use std::{env, path::PathBuf};
use tempfile::TempDir;

pub(super) const SCENARIOS: [&str; 18] = [
    "runtime-alpine",
    "copy",
    "buildcmd",
    "databases",
    "distros",
    "execcmd",
    "languages",
    "netcontainer",
    "network-contracts",
    "lifecycle",
    "permissions",
    "runflags",
    "terminal",
    "toolchains",
    "utilities",
    "volume-contracts",
    "web",
    "weird",
];

pub(super) async fn workflow() -> Result<(), Box<dyn std::error::Error>> {
    let name = env::args().nth(2).ok_or("workflow name is required")?;
    let work = TempDir::new()?;
    workflows::run(&name, &containers_for(work.path()).await?).await
}

pub(super) async fn quick() -> Result<(), Box<dyn std::error::Error>> {
    let mut results = Vec::with_capacity(SCENARIOS.len());
    record("runtime-alpine", runtime().await, &mut results);
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
    netcontainer::run(&containers, &rootfs).await?;
    coherence::run(&containers, &rootfs).await?;
    process::run(&containers, &rootfs).await?;
    lifecycle::run(&containers, &rootfs).await?;
    permissions::run(&containers).await?;
    utilities::run(&containers).await?;
    println!("PASS runtime-alpine");
    Ok(())
}

pub(super) async fn copy() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = containers_for(work.path()).await?;
    let target = crate::contract::Target::from_env()?;
    if target == crate::contract::Target::Arm64 {
        let rootfs = alpine(work.path()).await?;
        return copy::run(&containers, &rootfs).await;
    }
    let fixture =
        crate::fixture::Fixture::materialize_for("alpine:3.20", &target.platform()).await?;
    let result = copy::run(&containers, fixture.path()).await;
    let release = fixture.release();
    result?;
    release
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
    runner::Runner::from_env(&containers)?
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
    runner::Runner::from_env(&containers)?
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
