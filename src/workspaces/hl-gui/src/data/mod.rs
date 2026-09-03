//! Windowed data sources for collection components.
//!
//! A producer never ships whole result sets. It declares a source and answers
//! row windows the host asks for, so a million-row table costs one viewport.

mod cache;

pub use cache::{Lookup, RowCache};

use crate::render::SelectedRow;
use crate::style::{Align, Length, Tone};
use std::collections::BTreeSet;

/// Maximum columns one virtual table may allocate in a host renderer.
pub const TABLE_COLUMN_LIMIT: usize = 64;
/// Maximum UTF-8 bytes in a stable column identity.
pub const COLUMN_KEY_BYTE_LIMIT: usize = 128;
/// Maximum UTF-8 bytes in a user-visible column title.
pub const COLUMN_TITLE_BYTE_LIMIT: usize = 256;

/// Identity of one data source within a session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
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
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
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
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
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
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Column {
    #[cfg_attr(feature = "wire", serde(deserialize_with = "deserialize_column_key"))]
    pub key: String,
    #[cfg_attr(feature = "wire", serde(deserialize_with = "deserialize_column_title"))]
    pub title: String,
    pub width: Length,
    pub align: Align,
    pub sortable: bool,
    #[cfg_attr(feature = "wire", serde(default))]
    pub editable: bool,
}

impl Column {
    pub fn new(key: impl Into<String>, title: impl Into<String>) -> Self {
        let key = key.into();
        let title = title.into();
        assert!(valid_column_key(&key), "invalid table column key");
        assert!(valid_column_title(&title), "invalid table column title");
        Self {
            key,
            title,
            width: Length::Content,
            align: Align::Start,
            sortable: false,
            editable: false,
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

    #[must_use]
    pub const fn editable(mut self) -> Self {
        self.editable = true;
        self
    }
}

#[must_use]
pub fn valid_column_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= COLUMN_KEY_BYTE_LIMIT
}

#[must_use]
pub fn valid_column_title(value: &str) -> bool {
    !value.is_empty() && value.len() <= COLUMN_TITLE_BYTE_LIMIT
}

/// Validates a complete schema before it can allocate renderer columns.
pub fn validate_columns(columns: &[Column]) -> Result<(), &'static str> {
    if columns.len() > TABLE_COLUMN_LIMIT {
        return Err("table schema exceeds the column limit");
    }
    let mut keys = BTreeSet::new();
    for column in columns {
        if !valid_column_key(&column.key) {
            return Err("table column key is empty or exceeds its byte limit");
        }
        if !valid_column_title(&column.title) {
            return Err("table column title is empty or exceeds its byte limit");
        }
        if !keys.insert(column.key.as_str()) {
            return Err("table column keys must be unique");
        }
    }
    Ok(())
}

#[cfg(feature = "wire")]
fn deserialize_column_key<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    valid_column_key(&value)
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("column key must be 1..=128 UTF-8 bytes"))
}

#[cfg(feature = "wire")]
fn deserialize_column_title<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    valid_column_title(&value)
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("column title must be 1..=256 UTF-8 bytes"))
}

#[cfg(feature = "wire")]
pub(crate) fn deserialize_columns<'de, D>(deserializer: D) -> Result<Vec<Column>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let columns = <Vec<Column> as serde::Deserialize>::deserialize(deserializer)?;
    validate_columns(&columns).map_err(serde::de::Error::custom)?;
    Ok(columns)
}

/// One version-bound edit of a materialized virtual row.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionEdit {
    pub source: SourceId,
    pub version: Version,
    pub row: SelectedRow,
    pub column: String,
    pub value: String,
}

/// One rendered cell. Typed so alignment and formatting are the adapter's job,
/// not string formatting at the producer.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Cell {
    Text(String),
    Number(f64),
    Bytes(u64),
    Badge { label: String, tone: Tone },
    Stamp(i64),
    Empty,
}

impl Cell {
    /// Maximum UTF-8 payload retained or handed to a toolkit for one cell.
    pub const MAX_TEXT_BYTES: usize = 16 * 1024;

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

    #[must_use]
    fn text_bytes(&self) -> usize {
        match self {
            Self::Text(value) | Self::Badge { label: value, .. } => value.len(),
            Self::Number(_) | Self::Bytes(_) | Self::Stamp(_) | Self::Empty => 0,
        }
    }
}

/// One materialized row; cells match the declared columns positionally.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Row {
    pub key: u64,
    pub cells: Vec<Cell>,
}

impl Row {
    /// Maximum combined UTF-8 payload retained for one row.
    pub const MAX_TEXT_BYTES: usize = 64 * 1024;

    pub fn new(key: u64, cells: impl IntoIterator<Item = Cell>) -> Self {
        Self {
            key,
            cells: cells.into_iter().collect(),
        }
    }

    #[must_use]
    fn text_bytes(&self) -> Option<usize> {
        self.cells.iter().try_fold(0usize, |total, cell| {
            let bytes = cell.text_bytes();
            (bytes <= Cell::MAX_TEXT_BYTES).then(|| total.saturating_add(bytes))
        })
    }
}

/// A half-open span of absolute row indices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
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
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

/// Host request for one window of rows.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
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
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct RowWindow {
    pub source: SourceId,
    pub version: Version,
    pub request: RequestId,
    pub range: RowRange,
    pub rows: Vec<Row>,
}

impl RowWindow {
    /// Maximum combined UTF-8 payload accepted in one requested window.
    pub const MAX_TEXT_BYTES: usize = 256 * 1024;

    /// Whether textual cells fit the retained-model and toolkit rendering bounds.
    #[must_use]
    pub fn text_is_bounded(&self) -> bool {
        self.rows
            .iter()
            .try_fold(0usize, |total, row| {
                let bytes = row.text_bytes()?;
                (bytes <= Row::MAX_TEXT_BYTES).then(|| total.saturating_add(bytes))
            })
            .is_some_and(|bytes| bytes <= Self::MAX_TEXT_BYTES)
    }
}

/// A producer-side mutation of a source, independent of the node tree.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum SourceMutation {
    Open {
        source: SourceId,
        #[cfg_attr(feature = "wire", serde(deserialize_with = "deserialize_columns"))]
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
    use super::{
        validate_columns, Column, RowRange, COLUMN_KEY_BYTE_LIMIT, COLUMN_TITLE_BYTE_LIMIT, TABLE_COLUMN_LIMIT,
    };

    #[test]
    fn columns_are_read_only_unless_editing_is_explicit() {
        assert!(!Column::new("name", "Name").editable);
        assert!(Column::new("name", "Name").editable().editable);
    }

    #[test]
    fn table_schema_bounds_are_exact_and_keys_are_unique() {
        let columns = (0..TABLE_COLUMN_LIMIT)
            .map(|index| Column::new(format!("key-{index}"), "t".repeat(COLUMN_TITLE_BYTE_LIMIT)))
            .collect::<Vec<_>>();
        assert_eq!(columns[0].key.len(), "key-0".len());
        assert_eq!(
            Column::new("k".repeat(COLUMN_KEY_BYTE_LIMIT), "Title").key.len(),
            COLUMN_KEY_BYTE_LIMIT
        );
        assert_eq!(validate_columns(&columns), Ok(()));

        let mut overflow = columns.clone();
        overflow.push(Column::new("overflow", "Overflow"));
        assert!(validate_columns(&overflow).is_err());
        let mut duplicate = columns;
        duplicate[1].key = duplicate[0].key.clone();
        assert!(validate_columns(&duplicate).is_err());
    }

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
