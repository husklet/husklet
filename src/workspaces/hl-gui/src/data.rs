//! Windowed data sources for collection components.
//!
//! A producer never ships whole result sets. It declares a source and answers
//! row windows the host asks for, so a million-row table costs one viewport.

use crate::style::{Align, Length, Tone};

/// Identity of one data source within a session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceId(u64);

impl SourceId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Generation of a source's contents. A window carrying a stale generation is
/// discarded rather than shown next to fresh rows.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version(u64);

impl Version {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Identity of one outstanding window request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// One declared column of a table source.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub key: String,
    pub title: String,
    pub width: Length,
    pub align: Align,
    pub sortable: bool,
}

impl Column {
    pub fn new(key: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            width: Length::Content,
            align: Align::Start,
            sortable: false,
        }
    }

    #[must_use]
    pub const fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub const fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    #[must_use]
    pub const fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }
}

/// One rendered cell. Typed so alignment and formatting are the adapter's job,
/// not string formatting at the producer.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Text(String),
    Number(f64),
    Bytes(u64),
    Badge { label: String, tone: Tone },
    Stamp(i64),
    Empty,
}

impl Cell {
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    #[must_use]
    pub fn badge(label: impl Into<String>, tone: Tone) -> Self {
        Self::Badge {
            label: label.into(),
            tone,
        }
    }
}

/// One materialized row; cells match the declared columns positionally.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: u64,
    pub cells: Vec<Cell>,
}

impl Row {
    pub fn new(key: u64, cells: impl IntoIterator<Item = Cell>) -> Self {
        Self {
            key,
            cells: cells.into_iter().collect(),
        }
    }
}

/// A half-open span of absolute row indices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowRange {
    pub start: u64,
    pub count: u32,
}

impl RowRange {
    /// Rows per aligned request block. Scrolling far issues one request for the
    /// landing block rather than one per row crossed.
    pub const BLOCK: u32 = 128;

    #[must_use]
    pub const fn new(start: u64, count: u32) -> Self {
        Self { start, count }
    }

    /// The block-aligned range containing `index`.
    #[must_use]
    pub const fn block(index: u64) -> Self {
        let block = Self::BLOCK as u64;
        Self {
            start: index / block * block,
            count: Self::BLOCK,
        }
    }

    #[must_use]
    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.count as u64)
    }

    #[must_use]
    pub const fn contains(self, index: u64) -> bool {
        index >= self.start && index < self.end()
    }
}

/// Sort intent for a source, applied by the producer.
#[derive(Clone, Debug, PartialEq)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

/// Host request for one window of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct RowRequest {
    pub id: RequestId,
    pub source: SourceId,
    pub version: Version,
    pub range: RowRange,
    pub sort: Option<Sort>,
    pub filter: Option<String>,
}

/// Producer answer carrying the rows for one previously requested window.
#[derive(Clone, Debug, PartialEq)]
pub struct RowWindow {
    pub source: SourceId,
    pub version: Version,
    pub request: RequestId,
    pub range: RowRange,
    pub rows: Vec<Row>,
}

/// A producer-side mutation of a source, independent of the node tree.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DataOp {
    Open {
        source: SourceId,
        columns: Vec<Column>,
    },
    Length {
        source: SourceId,
        version: Version,
        rows: u64,
    },
    Window(RowWindow),
    /// Drop cached rows. `None` invalidates the whole source.
    Invalidate {
        source: SourceId,
        version: Version,
        range: Option<RowRange>,
    },
    Close {
        source: SourceId,
    },
}

#[cfg(test)]
mod tests {
    use super::RowRange;

    #[test]
    fn block_alignment_snaps_down_to_the_containing_block() {
        assert_eq!(RowRange::block(0).start, 0);
        assert_eq!(RowRange::block(127).start, 0);
        assert_eq!(RowRange::block(128).start, 128);
        assert_eq!(RowRange::block(40_000).start, 39_936);
    }

    #[test]
    fn ranges_report_membership_by_absolute_index() {
        let range = RowRange::new(128, 128);
        assert!(range.contains(128));
        assert!(range.contains(255));
        assert!(!range.contains(127));
        assert!(!range.contains(256));
    }
}
