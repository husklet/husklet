use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use hl_design_lint::{Cases, Diagnostic, Linter, Markdown, Reporter, Result, Severity};

#[derive(Debug, Parser)]
#[command(disable_version_flag = true)]
struct Arguments {
    #[arg(long, conflicts_with = "cases")]
    markdown: bool,
    #[arg(long, value_name = "DIRECTORY", conflicts_with = "markdown")]
    cases: Option<PathBuf>,
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

enum Output {
    Diagnostic,
    Markdown,
    Cases(PathBuf),
}

fn main() -> ExitCode {
    match Arguments::parse().run() {
        Ok(success) if success => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

impl Arguments {
    fn run(mut self) -> Result<bool> {
        let output = self.output();
        if self.paths.is_empty() {
            self.paths.push(PathBuf::from("src"));
        }

        let cases = matches!(output, Output::Cases(_));
        let mut reporter: Box<dyn Reporter> = match output {
            Output::Diagnostic => Box::new(Diagnostic::default()),
            Output::Markdown => Box::new(Markdown::default()),
            Output::Cases(root) => Box::new(Cases::new(root)),
        };
        let summaries = Linter::standard().run(self.paths, reporter.as_mut())?;
        Ok(cases
            || !summaries
                .iter()
                .any(|summary| summary.severity == Severity::Error && summary.findings != 0))
    }

    fn output(&mut self) -> Output {
        if let Some(root) = self.cases.take() {
            Output::Cases(root)
        } else if self.markdown {
            Output::Markdown
        } else {
            Output::Diagnostic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Arguments, Output};
    use clap::{Parser, error::ErrorKind};
    use std::path::PathBuf;

    #[test]
    fn legacy_modes_and_paths() {
        let mut markdown = Arguments::try_parse_from(["hl-design-lint", "--markdown", "src", "tests"]).unwrap();
        assert!(matches!(markdown.output(), Output::Markdown));
        assert_eq!(markdown.paths, [PathBuf::from("src"), PathBuf::from("tests")]);

        let mut cases = Arguments::try_parse_from(["hl-design-lint", "--cases", "lint", "src"]).unwrap();
        assert!(matches!(cases.output(), Output::Cases(path) if path == PathBuf::from("lint")));
        assert_eq!(cases.paths, [PathBuf::from("src")]);
    }

    #[test]
    fn repeated_paths() {
        let arguments = Arguments::try_parse_from(["hl-design-lint", "a", "b", "a"]).unwrap();
        assert_eq!(
            arguments.paths,
            [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("a")]
        );
    }

    #[test]
    fn missing_unknown_and_trailing() {
        let missing = Arguments::try_parse_from(["hl-design-lint", "--cases"]);
        assert_eq!(missing.unwrap_err().kind(), ErrorKind::InvalidValue);

        let unknown = Arguments::try_parse_from(["hl-design-lint", "--unknown"]);
        assert_eq!(unknown.unwrap_err().kind(), ErrorKind::UnknownArgument);

        let trailing = Arguments::try_parse_from(["hl-design-lint", "--", "--literal"]).unwrap();
        assert_eq!(trailing.paths, [PathBuf::from("--literal")]);
    }
}
