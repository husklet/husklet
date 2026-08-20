use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Diagnostic importance assigned by a rule.
pub enum Severity {
    /// Fails diagnostic-mode execution.
    Error,
    /// Requests review without failing execution.
    Warning,
}

impl Severity {
    /// Returns the diagnostic spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A quantity a rule measured against the largest value it accepts.
///
/// The count of findings a rule reports is not monotone in what the rule is about: a file taken
/// from 604 to 617 lines crosses the same single limit before and after. The measured quantity is,
/// so a rule that compares one against a limit carries both and lets the roll-up total the excess.
pub struct Budget {
    /// Singular noun for what is counted, such as `line` or `level`.
    pub unit: &'static str,
    /// Quantity the rule measured.
    pub measured: usize,
    /// Largest quantity the rule accepts.
    pub limit: usize,
}

impl Budget {
    /// Returns how far the measurement exceeds the limit.
    #[must_use]
    pub fn excess(self) -> usize {
        self.measured.saturating_sub(self.limit)
    }

    /// Adds another measurement of the same unit, keeping the total excess of both.
    fn total(&mut self, other: Self) {
        self.measured = self.measured.saturating_add(other.measured);
        self.limit = self.limit.saturating_add(other.limit);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Source location and its exact excerpt.
pub struct Location {
    /// Source file.
    pub path: PathBuf,
    /// One-based line.
    pub line: usize,
    /// One-based column.
    pub column: usize,
    /// Text covered by the reported span.
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Additional source context related to a finding.
pub struct Related {
    /// Relationship to the primary finding.
    pub label: String,
    /// Related source location.
    pub location: Location,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Queue destination for a review case.
pub enum ReviewState {
    /// Unresolved violation placed in `lint/errors`.
    Error,
    /// Temporary classification placed in `lint/check`.
    Check(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Human-review context attached by a rule.
pub struct Review {
    /// Target review queue.
    pub state: ReviewState,
    /// Rule-specific facts displayed in the case.
    pub metadata: Vec<(String, String)>,
    /// Calls or capabilities referenced by the finding.
    pub dependencies: Vec<String>,
    /// Questions the reviewer must answer.
    pub questions: Vec<String>,
}

impl Review {
    /// Creates an unresolved error review.
    #[must_use]
    pub fn error() -> Self {
        Self {
            state: ReviewState::Error,
            metadata: Vec::new(),
            dependencies: Vec::new(),
            questions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One rule result independent of its output format.
pub struct Finding {
    /// Stable rule identifier.
    pub rule: &'static str,
    /// Diagnostic severity.
    pub severity: Severity,
    /// Entity or operation under review.
    pub subject: String,
    /// Explanation of the violation.
    pub message: String,
    /// Actionable remediation guidance.
    pub help: String,
    /// Primary source location.
    pub location: Location,
    /// Related definitions or uses.
    pub related: Vec<Related>,
    /// Optional persistent review case.
    pub review: Option<Review>,
    /// Quantity and limit this finding compared, when the rule measures one.
    pub budget: Option<Budget>,
}

impl Finding {
    /// Creates an error finding.
    pub fn error(rule: &'static str, subject: impl Into<String>, location: Location) -> Self {
        Self::new(rule, Severity::Error, subject, location)
    }

    /// Creates a warning finding.
    pub fn warning(rule: &'static str, subject: impl Into<String>, location: Location) -> Self {
        Self::new(rule, Severity::Warning, subject, location)
    }

    fn new(rule: &'static str, severity: Severity, subject: impl Into<String>, location: Location) -> Self {
        Self {
            rule,
            severity,
            subject: subject.into(),
            message: String::new(),
            help: String::new(),
            location,
            related: Vec::new(),
            review: None,
            budget: None,
        }
    }

    /// Returns whether this finding contributes to rule failure.
    #[must_use]
    pub fn is_violation(&self) -> bool {
        !matches!(
            self.review.as_ref().map(|review| &review.state),
            Some(ReviewState::Check(_))
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Active finding count for one completed rule, beside how far its measurements overshoot.
pub struct Summary {
    /// Stable rule identifier.
    pub rule: &'static str,
    /// Rule severity.
    pub severity: Severity,
    /// Number of active violations.
    pub findings: usize,
    /// One totalled budget per unit the rule's active violations measured.
    pub budgets: Vec<Budget>,
}

impl Summary {
    /// Totals one rule's completed findings, keeping the excess each measured unit carries.
    #[must_use]
    pub fn new(rule: &'static str, severity: Severity, findings: &[Finding]) -> Self {
        let active = || findings.iter().filter(|finding| finding.is_violation());
        let mut budgets: Vec<Budget> = Vec::new();
        for budget in active().filter_map(|finding| finding.budget) {
            match budgets.iter_mut().find(|total| total.unit == budget.unit) {
                Some(total) => total.total(budget),
                None => budgets.push(budget),
            }
        }
        Self {
            rule,
            severity,
            findings: active().count(),
            budgets,
        }
    }
}
