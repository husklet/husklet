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

mod ab;
mod bench;
mod benchmark;
mod journal;
mod nested;
mod pool;
mod record;
mod runtime;
mod scenario;
mod suite;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "testing", about = "Husklet repository test runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run self-contained runtime compatibility cases.
    Runtime(runtime::Options),
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
    /// Run repository benchmark definitions.
    Bench(bench::Options),
    /// Compare two engine arms over one guest binary, serialized and order-balanced.
    Ab(ab::Options),
    /// Compare the retained C and Rust product backends with balanced order.
    ProductAb(bench::product_ab::Options),
    /// Build and freeze one runner/worker pair for later product A/B measurements.
    ProductAbPrepare(bench::product_ab::PrepareOptions),
    /// Run or report a direct provider benchmark.
    Benchmark {
        #[command(subcommand)]
        command: benchmark::Command,
    },
    /// Run nested-engine chains.
    Nested(nested::Options),
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
        Command::Runtime(options) => runtime::run(options).await,
        Command::RuntimeWorker(options) => runtime::worker(options).await,
        Command::Oracle(options) => runtime::oracle(options),
        Command::Scenarios(options) => scenario::run(options).await,
        Command::ScenarioInventory => scenario::inventory(),
        Command::ScenarioProvenance(options) => scenario::provenance(options),
        Command::ScenarioCachePreflight(options) => scenario::cache_preflight(options),
        Command::Bench(options) => bench::run(options).await,
        Command::Ab(options) => ab::run(options),
        Command::ProductAb(options) => bench::product_ab::run(options).await,
        Command::ProductAbPrepare(options) => bench::product_ab::prepare(options).await,
        Command::Benchmark { command } => benchmark::Application::new(std::env::var_os("PATH"))
            .execute(command)
            .map_err(Into::into),
        Command::Nested(options) => nested::run(options),
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
            "runtime",
            "oracle",
            "scenarios",
            "scenario-inventory",
            "scenario-provenance",
            "scenario-cache-preflight",
            "ab",
            "product-ab",
            "product-ab-prepare",
            "bench",
            "benchmark",
            "nested",
        ] {
            assert!(help.contains(command), "missing {command} from help");
        }
    }

    #[test]
    fn runtime_selection_parses() {
        assert!(Cli::try_parse_from(["testing", "runtime", "core", "--isa", "arm64"]).is_ok());
    }

    #[test]
    fn product_ab_requires_a_unique_result_destination() {
        assert!(
            Cli::try_parse_from([
                "testing",
                "product-ab",
                "lifecycle",
                "lifecycle",
                "--results",
                "target/testing/product-ab/run-1.tsv",
                "--artifacts",
                "target/testing/product-ab/artifacts/run-1",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["testing", "product-ab"]).is_err());
        assert!(
            Cli::try_parse_from([
                "testing",
                "product-ab",
                "lifecycle",
                "lifecycle",
                "--rounds",
                "3",
                "--results",
                "target/testing/product-ab/run-2.tsv",
                "--artifacts",
                "target/testing/product-ab/artifacts/run-2",
            ])
            .is_err()
        );
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

    #[test]
    fn benchmark_guest_arguments_are_trailing() {
        assert!(
            Cli::try_parse_from([
                "testing",
                "benchmark",
                "run",
                "--provider",
                "native",
                "--arch",
                "amd64",
                "--binary",
                "/guest",
                "--",
                "--phase",
                "compute"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "testing",
                "benchmark",
                "run",
                "--provider",
                "native",
                "--arch",
                "amd64",
                "--binary",
                "/guest",
                "--phase",
                "compute"
            ])
            .is_err()
        );
    }

    #[test]
    fn benchmark_architecture_spellings_are_explicit() {
        for isa in ["arm64", "amd64", "aarch64", "x86_64"] {
            assert!(
                Cli::try_parse_from([
                    "testing",
                    "benchmark",
                    "run",
                    "--provider",
                    "native",
                    "--arch",
                    isa,
                    "--binary",
                    "/guest"
                ])
                .is_ok(),
                "{isa}"
            );
        }
        assert!(
            Cli::try_parse_from([
                "testing",
                "benchmark",
                "run",
                "--provider",
                "native",
                "--arch",
                "x86",
                "--binary",
                "/guest"
            ])
            .is_err()
        );
    }
}
