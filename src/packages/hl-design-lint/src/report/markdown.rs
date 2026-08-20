use std::io::{self, Write};

use crate::{
    LintError, Result,
    model::{Finding, Summary},
    report::Reporter,
};

/// Emits a single Markdown review document.
pub struct Markdown<Output = io::Stdout> {
    output: Output,
    started: bool,
}

impl<Output> Markdown<Output> {
    /// Creates a Markdown reporter with an injected output.
    pub fn new(output: Output) -> Self {
        Self { output, started: false }
    }

    /// Returns the injected output.
    pub fn into_inner(self) -> Output {
        self.output
    }
}

impl Default for Markdown<io::Stdout> {
    fn default() -> Self {
        Self::new(io::stdout())
    }
}

impl<Output: Write> Reporter for Markdown<Output> {
    fn finding(&mut self, finding: &Finding) -> Result<()> {
        if !self.started {
            writeln!(self.output, "# Linting review\n")
                .map_err(|error| LintError::report("Markdown heading", error))?;
            self.started = true;
        }
        writeln!(
            self.output,
            "## `{}`\n\n- [ ] Reviewed\n- Rule: `{}`\n- Severity: `{}`\n- Location: `{}:{}:{}`\n- Violation: `{}`\n- Decision: \n\n{}\n\nHelp: {}\n\n````rust\n{}\n````\n",
            finding.subject,
            finding.rule,
            finding.severity.as_str(),
            finding.location.path.display(),
            finding.location.line,
            finding.location.column,
            finding.is_violation(),
            finding.message,
            finding.help,
            finding.location.source,
        )
        .map_err(|error| LintError::report("Markdown finding", error))?;
        for related in &finding.related {
            writeln!(
                self.output,
                "### {}\n\n- Location: `{}:{}:{}`\n\n````rust\n{}\n````\n",
                related.label,
                related.location.path.display(),
                related.location.line,
                related.location.column,
                related.location.source,
            )
            .map_err(|error| LintError::report("Markdown context", error))?;
        }
        Ok(())
    }

    fn finish(&mut self, summaries: &[Summary]) -> Result<()> {
        writeln!(self.output, "## Summary\n").map_err(|error| LintError::report("Markdown summary", error))?;
        for summary in summaries {
            write!(
                self.output,
                "- `{}`: {} {}(s)",
                summary.rule,
                summary.findings,
                summary.severity.as_str()
            )
            .map_err(|error| LintError::report("Markdown summary", error))?;
            for budget in &summary.budgets {
                write!(self.output, ", {} {}(s) over budget", budget.excess(), budget.unit)
                    .map_err(|error| LintError::report("Markdown summary", error))?;
            }
            writeln!(self.output).map_err(|error| LintError::report("Markdown summary", error))?;
        }
        Ok(())
    }
}
