use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use hl_design_lint::{Cases, Diagnostic, Linter, Markdown, Policy, Reporter, Result, Severity};

#[derive(Debug, Parser)]
#[command(disable_version_flag = true)]
struct Arguments {
    /// Runs the portable C rules on the requested paths.
    #[arg(long, conflicts_with_all = ["markdown", "cases"])]
    c: bool,
    /// Runs clang-format, clang-tidy, and cppcheck using a compilation database.
    #[arg(long, value_name = "DIRECTORY", conflicts_with_all = ["c", "markdown", "cases"])]
    c_analyzers: Option<PathBuf>,
    #[arg(long, default_value = "clang-format", requires = "c_analyzers")]
    clang_format: PathBuf,
    #[arg(long, default_value = "clang-tidy", requires = "c_analyzers")]
    clang_tidy: PathBuf,
    #[arg(long, default_value = "cppcheck", requires = "c_analyzers")]
    cppcheck: PathBuf,
    #[arg(long, value_name = "FILE")]
    policy: Option<PathBuf>,
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

        if let Some(compilation_database) = self.c_analyzers {
            let config = hl_design_lint::CAnalyzerConfig {
                clang_format: self.clang_format,
                clang_tidy: self.clang_tidy,
                cppcheck: self.cppcheck,
                compilation_database,
            };
            return hl_design_lint::run_c_analyzers(&config, &self.paths)
                .map_err(hl_design_lint::LintError::configuration);
        }

        let cases = matches!(output, Output::Cases(_));
        let mut reporter: Box<dyn Reporter> = match output {
            Output::Diagnostic => Box::new(Diagnostic::default()),
            Output::Markdown => Box::new(Markdown::default()),
            Output::Cases(root) => Box::new(Cases::new(root)),
        };
        let policy = self.policy.map(Policy::load).transpose()?.unwrap_or_default();
        let linter = if self.c {
            Linter::new(
                hl_design_lint::Registry::new()
                    .register(hl_design_lint::CInterface::new(policy.c_interface.clone()))
                    .register(hl_design_lint::CResult::new(policy.c_result.clone()))
                    .register(hl_design_lint::CStructure)
                    .register(hl_design_lint::CPolicy::new()),
            )
        } else {
            Linter::standard_with_policy(policy)
        };
        let summaries = linter.run(self.paths, reporter.as_mut())?;
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
        assert!(matches!(cases.output(), Output::Cases(path) if path == std::path::Path::new("lint")));
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

    #[test]
    fn c_analyzers_require_a_database_and_accept_tool_overrides() {
        let arguments = Arguments::try_parse_from([
            "hl-design-lint",
            "--c-analyzers",
            "build",
            "--clang-format",
            "format",
            "--clang-tidy",
            "tidy",
            "--cppcheck",
            "check",
            "native",
        ])
        .unwrap();
        assert_eq!(arguments.c_analyzers, Some(PathBuf::from("build")));
        assert_eq!(arguments.clang_format, PathBuf::from("format"));
        assert_eq!(arguments.clang_tidy, PathBuf::from("tidy"));
        assert_eq!(arguments.cppcheck, PathBuf::from("check"));
        assert_eq!(arguments.paths, [PathBuf::from("native")]);

        assert!(Arguments::try_parse_from(["hl-design-lint", "--clang-tidy", "tidy"]).is_err());
        assert!(Arguments::try_parse_from(["hl-design-lint", "--c", "--c-analyzers", "build"]).is_err());
    }
}
