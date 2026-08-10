use super::{definition::Benchmark, execution};
use crate::{
    runtime,
    suite::{Error, Target},
};
use clap::Args;
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const LEDGER_IDENTITY: &str = "husklet-product-ab-v1";
const STAGED: &str = "HL_PRODUCT_AB_STAGED";

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
        return stage_and_reexec(&workspace, options.target).await;
    }
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

async fn stage_and_reexec(workspace: &Path, target: Target) -> Result<(), Error> {
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

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let artifacts = workspace
        .join("target/testing/product-ab/artifacts")
        .join(format!("{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(
        artifacts
            .parent()
            .ok_or("product A/B artifact directory has no parent")?,
    )?;
    std::fs::create_dir(&artifacts)?;
    let source_runner = std::env::current_exe()?;
    let source_worker = workspace.join("target").join(profile).join(worker);
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
    eprintln!(
        "product-ab: artifacts={} runner_sha256={} worker={} worker_sha256={}",
        artifacts.display(),
        runner_identity,
        worker,
        worker_identity
    );

    let status = tokio::process::Command::new(&staged_runner)
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
    ) -> Result<(), Error> {
        writeln!(
            self.writer,
            "{LEDGER_IDENTITY}\tbenchmark={}\tcase={case}\ttarget={}\trounds={rounds}\tprovenance={identity}",
            benchmark.name,
            target.name()
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
    use super::{Ledger, parse_rounds, worker_name};
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
}
