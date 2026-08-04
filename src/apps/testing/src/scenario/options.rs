use super::definition::{Class, Scenario};
use crate::suite::Target;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named scenario.
    pub(super) scenario: Option<String>,
    /// Run only one full case ID within the selected scenario.
    #[arg(long = "case")]
    pub(super) case: Option<String>,
    /// Run only one guest ISA.
    #[arg(long = "isa", visible_alias = "target", value_enum)]
    target: Option<Target>,
    /// Run only quick or long cases.
    #[arg(long, value_enum)]
    pub(super) class: Option<Class>,
    /// Print selected case/target pairs without materializing images.
    #[arg(long)]
    pub(super) list: bool,
    /// Maximum number of concurrently executing cases.
    #[arg(long, env = "HL_COMPAT_JOBS", default_value_t = logical_jobs(), value_parser = parse_jobs)]
    pub(super) jobs: usize,
    /// Resume completed case/target keys from the durable partial result.
    #[arg(long, env = "HL_COMPAT_RESUME", default_value_t = false)]
    pub(super) resume: bool,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/scenarios/results.tsv", value_parser = parse_results)]
    pub(super) results: PathBuf,
}

fn logical_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(256)
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "jobs must be an integer".to_owned())?;
    (1..=256)
        .contains(&jobs)
        .then_some(jobs)
        .ok_or_else(|| "jobs must be between 1 and 256".to_owned())
}

fn parse_results(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        Err("results must be a safe relative path".to_owned())
    } else {
        Ok(path)
    }
}

impl Options {
    pub(super) fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }

    pub(super) fn select_cases(&self, mut scenarios: Vec<Scenario>) -> Result<Vec<Scenario>, String> {
        let Some(selected) = &self.case else {
            return Ok(scenarios);
        };
        for scenario in &mut scenarios {
            scenario.cases.retain(|case| case.id == *selected);
        }
        if scenarios.iter().all(|scenario| scenario.cases.is_empty()) {
            Err(format!("scenario case {selected} did not match the selected scenario"))
        } else {
            Ok(scenarios)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, logical_jobs, parse_jobs};
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        options: Options,
    }

    #[test]
    fn jobs_are_positive_and_bounded() {
        assert_eq!(parse_jobs("1"), Ok(1));
        assert_eq!(parse_jobs("256"), Ok(256));
        for invalid in ["0", "257", "many"] {
            assert!(parse_jobs(invalid).is_err(), "accepted {invalid}");
        }
        assert!((1..=256).contains(&logical_jobs()));
    }

    #[test]
    fn inventory_and_class_selection_are_typed() {
        let cli = TestCli::try_parse_from([
            "scenarios",
            "languages",
            "--case",
            "languages/perl-sum-538",
            "--class",
            "quick",
            "--list",
        ])
        .unwrap();
        assert!(cli.options.list);
        assert_eq!(cli.options.class, Some(super::Class::Quick));
        assert_eq!(cli.options.scenario.as_deref(), Some("languages"));
        assert_eq!(cli.options.case.as_deref(), Some("languages/perl-sum-538"));
        assert!(TestCli::try_parse_from(["scenarios", "--class", "smoke"]).is_err());
    }
}
