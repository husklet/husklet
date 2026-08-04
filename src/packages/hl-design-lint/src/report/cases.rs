use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    LintError, Result,
    model::{Finding, ReviewState, Summary},
    report::Reporter,
    source::{Workspace, domain, package, snake_case},
};

/// Replaces the flat `errors` and `check` Markdown review queues.
pub struct Cases<Output = io::Stderr> {
    root: PathBuf,
    written: usize,
    output: Output,
}

impl Cases<io::Stderr> {
    /// Creates a case reporter with status written to standard error.
    pub fn new(root: PathBuf) -> Self {
        Self::with_output(root, io::stderr())
    }
}

impl<Output> Cases<Output> {
    /// Creates a case reporter with an injected status output.
    pub fn with_output(root: PathBuf, output: Output) -> Self {
        Self {
            root,
            written: 0,
            output,
        }
    }

    /// Returns the injected status output.
    pub fn into_inner(self) -> Output {
        self.output
    }

    fn queue(&self, finding: &Finding) -> Option<(PathBuf, String)> {
        match &finding.review.as_ref()?.state {
            ReviewState::Error => Some((self.root.join("errors"), "unclassified".to_owned())),
            ReviewState::Check(classification) => Some((self.root.join("check"), classification.clone())),
        }
    }

    fn write(&self, finding: &Finding) -> Result<()> {
        let Some((queue, classification)) = self.queue(finding) else {
            return Ok(());
        };
        fs::create_dir_all(&queue).map_err(|error| LintError::io("create", &queue, error))?;
        let timestamp = timestamp();
        let domain = domain(&finding.location.path);
        let package = package(&finding.location.path)
            .map(|name| snake_case(&name))
            .unwrap_or_else(|| "unknown_package".to_owned());
        let output = queue.join(format!(
            "{}_{}_{}_{}.md",
            timestamp,
            domain,
            package,
            snake_case(&finding.subject)
        ));
        fs::write(
            &output,
            document(finding, timestamp, &domain, &package, &classification),
        )
        .map_err(|error| LintError::io("write", &output, error))
    }
}

impl<Output: Write> Reporter for Cases<Output> {
    fn begin(&mut self, _workspace: &Workspace) -> Result<()> {
        self.written = 0;
        for name in ["errors", "check"] {
            let queue = self.root.join(name);
            if queue.exists() {
                fs::remove_dir_all(&queue).map_err(|error| LintError::io("clear", &queue, error))?;
            }
            fs::create_dir_all(&queue).map_err(|error| LintError::io("create", &queue, error))?;
        }
        Ok(())
    }

    fn finding(&mut self, finding: &Finding) -> Result<()> {
        if finding.review.is_some() {
            self.write(finding)?;
            self.written += 1;
        }
        Ok(())
    }

    fn finish(&mut self, _summaries: &[Summary]) -> Result<()> {
        writeln!(self.output, "wrote {} case(s) to {}", self.written, self.root.display())
            .map_err(|error| LintError::report("case summary", error))
    }
}

fn document(finding: &Finding, timestamp: u64, domain: &str, package: &str, classification: &str) -> String {
    let review = finding.review.as_ref().expect("case finding has review");
    let metadata = review
        .metadata
        .iter()
        .map(|(name, value)| format!("- {name}: `{value}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let dependencies = if review.dependencies.is_empty() {
        "- None detected".to_owned()
    } else {
        review
            .dependencies
            .iter()
            .map(|dependency| format!("- `{dependency}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let questions = if review.questions.is_empty() {
        "- Confirm whether the finding represents the intended design.".to_owned()
    } else {
        review
            .questions
            .iter()
            .map(|question| format!("- {question}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let related = finding
        .related
        .iter()
        .map(|related| {
            format!(
                "### {}\n\n`{}:{}:{}`\n\n````rust\n{}\n````",
                related.label,
                related.location.path.display(),
                related.location.line,
                related.location.column,
                related.location.source
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "# `{}`\n\n- [ ] Approved\n- Timestamp: `{timestamp}`\n- Domain: `{domain}`\n- Package: `{package}`\n- Rule: `{}`\n- Severity: `{}`\n- Source: `{}:{}:{}`\n- Queue: `{classification}`\n{}\n\n## Finding\n\n{}\n\nHelp: {}\n\n## Review\n\n{}\n\n## Decision\n\n\n## Dependencies\n\n{}\n\n## Source\n\n````rust\n{}\n````\n\n## Related context\n\n{}\n",
        finding.subject,
        finding.rule,
        finding.severity.as_str(),
        finding.location.path.display(),
        finding.location.line,
        finding.location.column,
        metadata,
        finding.message,
        finding.help,
        questions,
        dependencies,
        finding.location.source,
        if related.is_empty() {
            "No related locations found in the scanned tree."
        } else {
            &related
        },
    )
}

fn timestamp() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut previous = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(previous.saturating_add(1));
        match LAST.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(current) => previous = current,
        }
    }
}
