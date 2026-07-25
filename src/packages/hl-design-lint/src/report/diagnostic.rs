use std::io::{self, Write};

use crate::{
    model::{Finding, Summary},
    report::Reporter,
    LintError, Result,
};

/// Emits compiler-style diagnostics and summaries.
pub struct Diagnostic<Output = io::Stderr> {
    output: Output,
}

impl<Output> Diagnostic<Output> {
    /// Creates a diagnostic reporter with an injected output.
    pub fn new(output: Output) -> Self {
        Self { output }
    }

    /// Returns the injected output.
    pub fn into_inner(self) -> Output {
        self.output
    }
}

impl Default for Diagnostic<io::Stderr> {
    fn default() -> Self {
        Self::new(io::stderr())
    }
}

impl<Output: Write> Reporter for Diagnostic<Output> {
    fn finding(&mut self, finding: &Finding) -> Result<()> {
        if !finding.is_violation() {
            return Ok(());
        }
        writeln!(
            self.output,
            "{}[{}]: {}\n  --> {}:{}:{}\n   = help: {}",
            finding.severity.as_str(),
            finding.rule,
            finding.message,
            finding.location.path.display(),
            finding.location.line,
            finding.location.column,
            finding.help,
        )
        .map_err(|error| LintError::report("diagnostic", error))?;
        for related in &finding.related {
            writeln!(
                self.output,
                "   = {}: {}:{}:{}",
                related.label,
                related.location.path.display(),
                related.location.line,
                related.location.column,
            )
            .map_err(|error| LintError::report("diagnostic", error))?;
        }
        Ok(())
    }

    fn finish(&mut self, summaries: &[Summary]) -> Result<()> {
        for summary in summaries {
            writeln!(
                self.output,
                "{}: {} {}(s)",
                summary.rule,
                summary.findings,
                summary.severity.as_str()
            )
            .map_err(|error| LintError::report("diagnostic summary", error))?;
        }
        Ok(())
    }
}
