use super::{definition::Benchmark, execution};
use crate::{
    runtime,
    suite::{Error, Target},
};
use clap::Args;
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

const LEDGER_IDENTITY: &str = "husklet-product-ab-v2";
const STAGED: &str = "HL_PRODUCT_AB_STAGED";
const ARTIFACT_IDENTITY: &str = "husklet-product-ab-artifacts-v1";

#[derive(Args)]
pub(crate) struct PrepareOptions {
    /// Guest ISA whose architecture worker is staged.
    #[arg(long = "isa", value_enum, default_value = "arm64")]
    target: Target,
    /// New, never-reused artifact directory beneath the repository workspace.
    #[arg(long, required = true, value_parser = crate::suite::parse::results)]
    artifacts: PathBuf,
}

#[derive(Args)]
pub(crate) struct Options {
    /// Benchmark definition name beneath tests/bench.
    #[arg(default_value = "lifecycle")]
    benchmark: String,
    /// Case id within the benchmark definition.
    #[arg(default_value = "lifecycle")]
    case: String,
    /// Guest ISA (the retained C backend currently supports arm64).
    #[arg(long = "isa", value_enum, default_value = "arm64")]
    target: Target,
    /// Number of measured pairs; must be even to balance first position.
    #[arg(long, default_value_t = 6, value_parser = parse_rounds)]
    rounds: u32,
    /// New, never-reused result path beneath the repository workspace.
    #[arg(long, required = true, value_parser = crate::suite::parse::results)]
    results: PathBuf,
    /// Artifact directory created by `product-ab-prepare` before the quiet window.
    #[arg(long, required = true, value_parser = crate::suite::parse::results)]
    artifacts: PathBuf,
}

fn parse_rounds(value: &str) -> Result<u32, String> {
    let rounds = value.parse::<u32>().map_err(|_| "rounds must be an even integer")?;
    if !(2..=100).contains(&rounds) || rounds % 2 != 0 {
        return Err("rounds must be an even integer from 2 through 100".into());
    }
    Ok(rounds)
}

pub(crate) async fn run(options: Options) -> Result<(), Error> {
    let workspace = runtime::workspace()?;
    if std::env::var_os(STAGED).is_none() {
        return reexec(&workspace, options.target, &options.artifacts).await;
    }
    let artifacts = verify_artifacts(&workspace.join(&options.artifacts), options.target, true)?;
    let directory = workspace.join("tests/bench").join(&options.benchmark);
    let benchmark = Arc::new(Benchmark::load(&directory, &directory.join("test.yaml"))?);
    let case_index = benchmark
        .cases
        .iter()
        .position(|case| case.id == options.case)
        .ok_or_else(|| format!("benchmark {} has no case {}", options.benchmark, options.case))?;
    let result_path = workspace.join(&options.results);
    let mut ledger = Ledger::create(&result_path)?;
    let prepared = execution::prepare(&benchmark, case_index, options.target).await?;
    ledger.header(
        &benchmark,
        &options.case,
        options.target,
        options.rounds,
        &prepared.identity,
        &artifacts,
    )?;
    let run = execution::run_product_ab(benchmark, case_index, options.target, prepared, options.rounds).await?;
    ledger.setup(&run.setup)?;
    for sample in &run.samples {
        ledger.sample(sample)?;
    }
    ledger.finish()?;
    println!(
        "product-ab: wrote {} balanced samples to {}",
        run.samples.len(),
        result_path.display()
    );
    Ok(())
}

pub(crate) async fn prepare(options: PrepareOptions) -> Result<(), Error> {
    let workspace = runtime::workspace()?;
    stage(&workspace, options.target, &options.artifacts).await
}

async fn stage(workspace: &Path, target: Target, relative: &Path) -> Result<(), Error> {
    let worker = worker_name(target);
    let profile = crate::runtime::profile::PROFILE;
    let mut build = tokio::process::Command::new(env!("CARGO"));
    build
        .current_dir(workspace)
        .args(["build", "-p", "engine", "--bin", worker]);
    if profile == "release" {
        build.arg("--release");
    }
    let status = build.status().await?;
    if !status.success() {
        return Err(format!("building product A/B worker {worker} failed with {status}").into());
    }

    let artifacts = workspace.join(relative);
    std::fs::create_dir_all(
        artifacts
            .parent()
            .ok_or("product A/B artifact directory has no parent")?,
    )?;
    std::fs::create_dir(&artifacts)?;
    let source_runner = std::env::current_exe()?;
    let source_worker = source_runner
        .parent()
        .ok_or("product A/B runner has no binary directory")?
        .join(worker);
    let staged_runner = artifacts.join("testing");
    let staged_worker = artifacts.join(worker);

    // Keep the two copies in distinct completed phases. A worker is never observed while
    // either source or destination is still changing.
    copy_artifact(source_runner.clone(), staged_runner.clone()).await?;
    copy_artifact(source_worker.clone(), staged_worker.clone()).await?;
    smoke(&staged_runner, Some("--help"), &[0]).await?;
    // With no guest argument the architecture worker deliberately returns its
    // bounded usage status. Reaching that status proves the copied ELF executes.
    smoke(&staged_worker, None, &[64]).await?;
    let runner_identity = crate::record::FramedIdentity::of_file(&staged_runner)?;
    let worker_identity = crate::record::FramedIdentity::of_file(&staged_worker)?;
    let manifest = format!(
        "{ARTIFACT_IDENTITY}\ntarget={}\nrunner_sha256={runner_identity}\nworker={worker}\nworker_sha256={worker_identity}\n",
        target.name()
    );
    let manifest_path = artifacts.join("manifest.tsv");
    let mut manifest_file = OpenOptions::new().write(true).create_new(true).open(&manifest_path)?;
    manifest_file.write_all(manifest.as_bytes())?;
    manifest_file.sync_all()?;
    std::fs::set_permissions(&staged_runner, std::fs::Permissions::from_mode(0o555))?;
    std::fs::set_permissions(&staged_worker, std::fs::Permissions::from_mode(0o555))?;
    std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o444))?;
    std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o555))?;
    eprintln!(
        "product-ab: artifacts={} runner_sha256={} worker={} worker_sha256={}",
        artifacts.display(),
        runner_identity,
        worker,
        worker_identity
    );

    Ok(())
}

async fn reexec(workspace: &Path, target: Target, relative: &Path) -> Result<(), Error> {
    let artifacts = workspace.join(relative);
    let staged = verify_artifacts(&artifacts, target, false)?;
    let status = tokio::process::Command::new(&staged.runner)
        .args(std::env::args_os().skip(1))
        .env(STAGED, "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("staged product A/B runner failed with {status}").into())
    }
}

struct Artifacts {
    runner: PathBuf,
    runner_identity: String,
    worker_identity: String,
}

fn verify_artifacts(artifacts: &Path, target: Target, require_current_runner: bool) -> Result<Artifacts, Error> {
    let worker = worker_name(target);
    let manifest = std::fs::read_to_string(artifacts.join("manifest.tsv"))?;
    let lines = manifest.lines().collect::<Vec<_>>();
    if lines.len() != 5
        || lines[0] != ARTIFACT_IDENTITY
        || lines[1] != format!("target={}", target.name())
        || lines[3] != format!("worker={worker}")
    {
        return Err(format!("invalid product A/B artifact manifest at {}", artifacts.display()).into());
    }
    let runner = artifacts.join("testing");
    let staged_worker = artifacts.join(worker);
    let runner_identity = crate::record::FramedIdentity::of_file(&runner)?;
    let worker_identity = crate::record::FramedIdentity::of_file(&staged_worker)?;
    if lines[2] != format!("runner_sha256={runner_identity}") || lines[4] != format!("worker_sha256={worker_identity}")
    {
        return Err(format!(
            "product A/B artifacts changed after preparation: {}",
            artifacts.display()
        )
        .into());
    }
    if require_current_runner && std::env::current_exe()?.canonicalize()? != runner.canonicalize()? {
        return Err("HL_PRODUCT_AB_STAGED may only be set by the staged runner".into());
    }
    Ok(Artifacts {
        runner,
        runner_identity,
        worker_identity,
    })
}

async fn copy_artifact(source: PathBuf, destination: PathBuf) -> Result<(), Error> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        std::fs::copy(&source, &destination).map_err(|error| error.to_string())?;
        let permissions = std::fs::metadata(&source)
            .map_err(|error| error.to_string())?
            .permissions();
        std::fs::set_permissions(&destination, permissions).map_err(|error| error.to_string())?;
        Ok(())
    })
    .await?
    .map_err(|error| -> Error { error.into() })?;
    Ok(())
}

async fn smoke(executable: &Path, argument: Option<&str>, expected: &[i32]) -> Result<(), Error> {
    let mut command = tokio::process::Command::new(executable);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(argument) = argument {
        command.arg(argument);
    }
    let status = command.status().await?;
    if status.code().is_some_and(|code| expected.contains(&code)) {
        Ok(())
    } else {
        Err(format!(
            "staged product A/B artifact {} failed smoke run with {status}",
            executable.display()
        )
        .into())
    }
}

const fn worker_name(target: Target) -> &'static str {
    match target {
        Target::Arm64 => "hl-aarch64",
        Target::Amd64 => "hl-x86_64",
    }
}

struct Ledger {
    writer: BufWriter<std::fs::File>,
}

impl Ledger {
    fn create(path: &std::path::Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!("refusing to reuse product A/B ledger {}", path.display()).into()
                } else {
                    Error::from(error)
                }
            })?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn header(
        &mut self,
        benchmark: &Benchmark,
        case: &str,
        target: Target,
        rounds: u32,
        identity: &str,
        artifacts: &Artifacts,
    ) -> Result<(), Error> {
        writeln!(
            self.writer,
            "{LEDGER_IDENTITY}\tbenchmark={}\tcase={case}\ttarget={}\trounds={rounds}\tprovenance={identity}\trunner_sha256={}\tworker_sha256={}",
            benchmark.name,
            target.name(),
            artifacts.runner_identity,
            artifacts.worker_identity
        )?;
        writeln!(
            self.writer,
            "kind\tround\tposition\tbackend\tsetup_us\texecution_us\tteardown_us\ttotal_us\toutput_identity"
        )?;
        self.sync()
    }

    fn setup(&mut self, setup: &std::collections::BTreeMap<String, u128>) -> Result<(), Error> {
        for (name, elapsed) in setup {
            writeln!(self.writer, "setup\t-\t-\t{name}\t{elapsed}\t-\t-\t-\t-")?;
        }
        self.sync()
    }

    fn sample(&mut self, sample: &execution::ProductSample) -> Result<(), Error> {
        writeln!(
            self.writer,
            "sample\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            sample.round,
            sample.position,
            sample.backend.name(),
            sample.setup_us,
            sample.execution_us,
            sample.teardown_us,
            sample.total_us,
            sample.output_identity
        )?;
        self.sync()
    }

    fn sync(&mut self) -> Result<(), Error> {
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        Ok(())
    }

    fn finish(mut self) -> Result<(), Error> {
        writeln!(self.writer, "finish")?;
        self.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::{ARTIFACT_IDENTITY, Ledger, parse_rounds, verify_artifacts, worker_name};
    use crate::suite::Target;

    #[test]
    fn rounds_must_balance_first_position() {
        assert_eq!(parse_rounds("6"), Ok(6));
        assert!(parse_rounds("1").is_err());
        assert!(parse_rounds("3").is_err());
        assert!(parse_rounds("102").is_err());
    }

    #[test]
    fn ledger_refuses_an_existing_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.tsv");
        let _first = Ledger::create(&path).unwrap();
        assert!(Ledger::create(&path).is_err());
    }

    #[test]
    fn staged_worker_matches_the_exact_guest_architecture() {
        assert_eq!(worker_name(Target::Arm64), "hl-aarch64");
        assert_eq!(worker_name(Target::Amd64), "hl-x86_64");
    }

    #[test]
    fn artifact_manifest_pins_runner_and_worker_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let runner = directory.path().join("testing");
        let worker = directory.path().join("hl-aarch64");
        std::fs::write(&runner, b"runner").unwrap();
        std::fs::write(&worker, b"worker").unwrap();
        let runner_identity = crate::record::FramedIdentity::of_file(&runner).unwrap();
        let worker_identity = crate::record::FramedIdentity::of_file(&worker).unwrap();
        std::fs::write(
            directory.path().join("manifest.tsv"),
            format!(
                "{ARTIFACT_IDENTITY}\ntarget=arm64\nrunner_sha256={runner_identity}\nworker=hl-aarch64\nworker_sha256={worker_identity}\n"
            ),
        )
        .unwrap();

        assert_eq!(
            verify_artifacts(directory.path(), Target::Arm64, false).unwrap().runner,
            runner
        );
        std::fs::write(worker, b"changed").unwrap();
        assert!(verify_artifacts(directory.path(), Target::Arm64, false).is_err());
    }
}
