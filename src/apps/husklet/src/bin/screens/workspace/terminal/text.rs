//! Reading what a pane is showing, as text.
//!
//! Both readers here bound the rows before any text is extracted: a session
//! dump has to fit on disk, and an extension's read has to fit in one answer.
//! An unbounded extraction would let whatever a shell printed decide how much
//! the host allocates.

use super::*;

const SAVED_HISTORY_LINES: usize = 5000;

fn history_row_range(first: i64, last: i64, maximum: usize) -> (i64, i64) {
    let last = last.max(first);
    let maximum = i64::try_from(maximum).unwrap_or(i64::MAX);
    (last.saturating_sub(maximum).max(first), last)
}

impl Terminal<'_> {
    /// A bounded tail of what the pane is showing, oldest line first.
    ///
    /// The bound is applied to the rows *before* the text is extracted, so a
    /// pane whose shell printed a gigabyte never has a gigabyte pulled out of
    /// it to be cut afterwards. The flag says whether older rows were left.
    pub(crate) fn tail(&self, lines: usize) -> (Vec<String>, bool) {
        let terminal = self.0;
        let (first, last) = match terminal.vadjustment() {
            Some(adjustment) => (adjustment.lower() as i64, adjustment.upper() as i64),
            None => (0, terminal.row_count()),
        };
        let (start, end) = history_row_range(first, last, lines);
        let (text, _length) = terminal.text_range_format(vte4::Format::Text, start, 0, end, -1);
        let raw = text.map(|extracted| extracted.to_string()).unwrap_or_default();
        (Self::rows(&raw, lines), start > first)
    }

    /// Extracted text as at most `lines` lines, with the blank tail a terminal
    /// screen always carries dropped.
    fn rows(text: &str, lines: usize) -> Vec<String> {
        let trimmed = text.trim_end_matches(['\n', '\r', ' ', '\t']);
        if trimmed.is_empty() {
            return Vec::new();
        }
        let all: Vec<String> = trimmed
            .split('\n')
            .map(|line| line.trim_end_matches('\r').to_owned())
            .collect();
        all[all.len().saturating_sub(lines)..].to_vec()
    }

    /// Extract the whole scrollback and visible screen as plain text.
    pub(crate) fn history(&self) -> String {
        let term = self.0;
        // The vadjustment spans the whole buffer: value range [lower, upper); rows are 1:1 with it.
        let (first, last) = match term.vadjustment() {
            Some(adj) => (adj.lower() as i64, adj.upper() as i64),
            None => (0, term.row_count()),
        };
        let (first, last) = history_row_range(first, last, SAVED_HISTORY_LINES);
        let (text, _len) = term.text_range_format(vte4::Format::Text, first, 0, last, -1);
        let raw = text.map(|g| g.to_string()).unwrap_or_default();
        // Cap the persisted history so a huge scrollback doesn't bloat the session on disk.
        session::History::new(&raw).clamp(SAVED_HISTORY_LINES)
    }
}

#[cfg(test)]
mod history_tests {
    use super::history_row_range;

    #[test]
    fn persisted_history_extracts_only_the_bounded_scrollback_tail() {
        assert_eq!(history_row_range(0, 1_000_000, 5000), (995_000, 1_000_000));
        assert_eq!(history_row_range(40, 80, 5000), (40, 80));
        assert_eq!(history_row_range(80, 40, 5000), (80, 80));
    }
}
