use super::definition::{Class, Scenario};
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named scenario.
    pub(super) scenario: Option<String>,
    #[command(flatten)]
    pub(super) selection: crate::suite::Selection,
    /// Run only quick or long cases.
    #[arg(long, value_enum)]
    pub(super) class: Option<Class>,
    /// Print selected case/target pairs without materializing images.
    #[arg(long)]
    pub(super) list: bool,
    /// Reuse provider services while retaining a fresh image view and container per case.
    #[arg(long, default_value_t = false)]
    pub(super) warm_provider: bool,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/scenarios/results.tsv", value_parser = crate::suite::parse::results)]
    pub(super) results: PathBuf,
}

impl Options {
    pub(super) fn select_cases(&self, mut scenarios: Vec<Scenario>) -> Result<Vec<Scenario>, String> {
        let Some(selected) = &self.selection.case else {
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
    use super::Options;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        options: Options,
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
        assert_eq!(cli.options.selection.case.as_deref(), Some("languages/perl-sum-538"));
        assert!(TestCli::try_parse_from(["scenarios", "--class", "smoke"]).is_err());
        let warm = TestCli::try_parse_from(["scenarios", "--warm-provider"]).unwrap();
        assert!(warm.options.warm_provider);
    }
}
