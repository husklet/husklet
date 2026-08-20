// Fixtures and reporters hand their helpers the owned records they assert on, keep a receiver so
// each harness family reads uniformly, and build report text by concatenation.
#![allow(
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::large_futures,
    clippy::unnecessary_wraps,
    clippy::type_complexity,
    clippy::large_enum_variant,
    clippy::field_reassign_with_default
)]
#![forbid(unsafe_code)]

mod benchmark;
mod journal;
mod leaks;
mod nested;
mod platform;
mod pool;
mod record;
mod runtime;
mod scenario;
mod suite;
mod syscall_audit;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "testing", about = "Husklet repository test runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a controlled C-engine performance acceptance campaign.
    Benchmark(benchmark::Options),
    /// Calibrate benchmark noise using identical baseline arms.
    BenchmarkCalibrate(benchmark::CalibrationOptions),
    /// Measure the near-native performance floor: per-process, per-crossing, and control.
    BenchmarkFloor(benchmark::FloorOptions),
    /// Print the exact artifact identity accepted by a benchmark campaign.
    BenchmarkHash(benchmark::HashOptions),
    /// Stage content-bound artifacts for a real three-arm benchmark campaign.
    BenchmarkStage(benchmark::StageOptions),
    /// Run self-contained runtime compatibility cases.
    Runtime(runtime::Options),
    /// Print the exact current-host runtime corpus plan without executing cases.
    RuntimeInventory,
    /// Build and publish an immutable runtime-corpus runner and native library.
    RuntimeStage(runtime::StageOptions),
    #[command(hide = true)]
    RuntimeWorker(runtime::WorkerOptions),
    /// Check or update runtime output using the configured oracle.
    Oracle(runtime::OracleOptions),
    /// Run application scenarios.
    Scenarios(scenario::Options),
    /// Report the complete YAML scenario inventory without executing it.
    ScenarioInventory,
    /// Audit YAML case identity, image, target, and action provenance.
    ScenarioProvenance(scenario::ProvenanceOptions),
    /// Verify that every selected scenario image exists in the exact offline cache.
    ScenarioCachePreflight(scenario::CachePreflightOptions),
    /// Run nested-engine chains.
    Nested(nested::Options),
    /// Audit production syscall coverage against the typed router inventory.
    SyscallAudit(syscall_audit::Options),
    /// Check the production C engine with the platform leak detector.
    Leaks(leaks::Options),
    #[command(hide = true)]
    LeakProbe,
    #[command(hide = true)]
    NativeArtifactSmoke,
}

#[tokio::main]
async fn main() {
    let defaults = hl_log::Config {
        logging: hl_log::tag::EXEC.into(),
        level: hl_log::Level::Error,
        profiling: hl_log::Tags::NONE,
    };
    let logging = hl_log::EnvironmentConfig::parse(defaults, std::env::vars());
    for warning in logging.warnings() {
        eprintln!("testing: {warning}");
    }
    logging.apply();
    if let Err(error) = run().await {
        eprintln!("testing: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Benchmark(options) => benchmark::run(options),
        Command::BenchmarkCalibrate(options) => benchmark::calibrate(options),
        Command::BenchmarkFloor(options) => benchmark::floor(options),
        Command::BenchmarkHash(options) => benchmark::hash(options),
        Command::BenchmarkStage(options) => benchmark::stage(options),
        Command::Runtime(options) => runtime::run(options).await,
        Command::RuntimeInventory => runtime::inventory(),
        Command::RuntimeStage(options) => runtime::stage(options),
        Command::RuntimeWorker(options) => runtime::worker(options).await,
        Command::Oracle(options) => runtime::oracle(options),
        Command::Scenarios(options) => scenario::run(options).await,
        Command::ScenarioInventory => scenario::inventory(),
        Command::ScenarioProvenance(options) => scenario::provenance(options),
        Command::ScenarioCachePreflight(options) => scenario::cache_preflight(options),
        Command::Nested(options) => nested::run(options),
        Command::SyscallAudit(options) => syscall_audit::run(options),
        Command::Leaks(options) => leaks::run(options),
        Command::LeakProbe => {
            let _ = hl_native::leak_check_nonvacuity();
            Ok(())
        }
        Command::NativeArtifactSmoke => {
            runtime::artifact_smoke()?;
            println!("hl-native-artifact-smoke-v1");
            Ok(())
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::Cli;
    use clap::{CommandFactory, Parser};

    #[test]
    fn help_exposes_all_typed_commands() {
        let help = Cli::command().render_long_help().to_string();
        for command in [
            "benchmark",
            "benchmark-calibrate",
            "benchmark-hash",
            "benchmark-stage",
            "runtime",
            "runtime-inventory",
            "runtime-stage",
            "oracle",
            "scenarios",
            "scenario-inventory",
            "scenario-provenance",
            "scenario-cache-preflight",
            "nested",
            "syscall-audit",
            "leaks",
        ] {
            assert!(help.contains(command), "missing {command} from help");
        }
    }

    #[test]
    fn runtime_selection_parses() {
        assert!(Cli::try_parse_from(["testing", "runtime", "core", "--isa", "arm64"]).is_ok());
    }

    #[test]
    fn scenario_ci_replacements_are_typed() {
        assert!(Cli::try_parse_from(["testing", "scenario-inventory"]).is_ok());
        assert!(Cli::try_parse_from(["testing", "scenario-provenance", "--details"]).is_ok());
        assert!(Cli::try_parse_from(["testing", "scenario-cache-preflight", "arm64"]).is_ok());
        assert!(Cli::try_parse_from(["testing", "scenario-cache-preflight", "x86"]).is_err());
        assert!(Cli::try_parse_from(["testing", "scenarios", "--target", "amd64", "--list"]).is_ok());
    }

    #[test]
    fn nested_preparation_and_execution_are_typed() {
        assert!(Cli::try_parse_from(["testing", "nested", "prepare"]).is_ok());
        assert!(Cli::try_parse_from(["testing", "nested", "run", "tests/runtime/nested/chains.yaml"]).is_ok());
        assert!(Cli::try_parse_from(["testing", "nested", "prepare", "--shell", "make foreign"]).is_err());
    }

    #[test]
    fn invalid_isa_and_conflicting_oracle_modes_fail() {
        assert!(Cli::try_parse_from(["testing", "runtime", "--isa", "x86"]).is_err());
        assert!(Cli::try_parse_from(["testing", "oracle", "--check", "--update"]).is_err());
    }

    #[test]
    fn missing_and_unknown_commands_are_usage_errors() {
        assert!(Cli::try_parse_from(["testing"]).is_err());
        assert!(Cli::try_parse_from(["testing", "unknown"]).is_err());
    }
}
