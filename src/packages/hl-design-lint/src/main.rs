use std::{env, path::PathBuf, process::ExitCode};

use hl_design_lint::{Cases, Diagnostic, LintError, Linter, Markdown, Reporter, Result, Severity};

enum Output {
    Diagnostic,
    Markdown,
    Cases(PathBuf),
}

fn main() -> ExitCode {
    match run(env::args_os().skip(1)) {
        Ok(success) if success => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Result<bool> {
    let mut output = Output::Diagnostic;
    let mut paths = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "--markdown" {
            output = Output::Markdown;
        } else if argument == "--cases" {
            output = Output::Cases(PathBuf::from(
                arguments
                    .next()
                    .ok_or(LintError::Argument("--cases requires an output directory"))?,
            ));
        } else {
            paths.push(PathBuf::from(argument));
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("src"));
    }

    let cases = matches!(output, Output::Cases(_));
    let mut reporter: Box<dyn Reporter> = match output {
        Output::Diagnostic => Box::new(Diagnostic::default()),
        Output::Markdown => Box::new(Markdown::default()),
        Output::Cases(root) => Box::new(Cases::new(root)),
    };
    let summaries = Linter::standard().run(paths, reporter.as_mut())?;
    Ok(cases
        || !summaries
            .iter()
            .any(|summary| summary.severity == Severity::Error && summary.findings != 0))
}
