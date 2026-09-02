//! The windowing cache: what is held, what is asked for, and what is given up on.
//!
//! This is the whole reason a large source is affordable. A viewport lookup
//! must answer immediately — a renderer cannot wait on a producer — so a miss
//! returns a placeholder and schedules a request instead of blocking. Time is
//! supplied by the caller rather than read from a clock, so every deadline and
//! retry here is deterministic under test.

use std::collections::BTreeMap;

use super::{RequestId, Row, RowRange, RowRequest, RowWindow, Sort, SourceId, Version};

/// How a viewport lookup resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lookup<'a> {
    /// Cached and current.
    Ready(&'a Row),
    /// Requested, or about to be. Render a placeholder.
    Pending,
    /// The producer failed or gave up. Render an unavailable marker.
    Unavailable,
    /// Past the end of the source.
    Absent,
}

/// State of one aligned block of rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Block {
    /// Not held, carrying how many attempts have already been spent on it so a
    /// retried block cannot reset its own budget and retry forever.
    Missing {
        attempts: u8,
    },
    Requested {
        id: RequestId,
        issued: u64,
        attempts: u8,
    },
    Ready,
    Unavailable,
}

/// Holds a bounded window of one source's rows and decides what to request.
#[derive(Debug)]
pub struct RowCache {
    source: SourceId,
    version: Version,
    rows: BTreeMap<u64, Row>,
    blocks: BTreeMap<u64, Block>,
    recency: BTreeMap<u64, u64>,
    viewport: RowRange,
    length: Option<u64>,
    sort: Option<Sort>,
    filter: Option<String>,
    next_request: u64,
    tick: u64,
}

impl RowCache {
    /// Requests outstanding at once. A fast scroll over a slow producer must
    /// leave one request for where the user landed, not a queue of history.
    pub const IN_FLIGHT_LIMIT: usize = 4;
    /// Blocks fetched either side of the viewport.
    pub const PREFETCH_BLOCKS: u64 = 1;
    /// Rows retained before eviction begins.
    pub const CAPACITY: usize = 4096;
    /// After this, a pending block is reported as slow but still expected.
    pub const SOFT_DEADLINE: u64 = 150;
    /// After this, a request is abandoned and retried once.
    pub const HARD_DEADLINE: u64 = 2_000;
    /// Attempts before a block is declared unavailable.
    pub const ATTEMPT_LIMIT: u8 = 2;

    #[must_use]
    pub fn new(source: SourceId) -> Self {
        Self {
            source,
            version: Version::default(),
            rows: BTreeMap::new(),
            blocks: BTreeMap::new(),
            recency: BTreeMap::new(),
            viewport: RowRange::new(0, 0),
            length: None,
            sort: None,
            filter: None,
            next_request: 1,
            tick: 0,
        }
    }

    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    /// Row count the producer last reported, if any.
    #[must_use]
    pub const fn length(&self) -> Option<u64> {
        self.length
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Requests awaiting an answer.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.blocks
            .values()
            .filter(|block| matches!(block, Block::Requested { .. }))
            .count()
    }

    /// Records a new row count, which supersedes everything cached.
    pub fn resize(&mut self, version: Version, rows: u64) {
        if version > self.version {
            self.discard();
            self.version = version;
        }
        self.length = Some(rows);
    }

    /// Answers a viewport lookup without ever waiting on the producer.
    #[must_use]
    pub fn row(&self, index: u64) -> Lookup<'_> {
        if self.length.is_some_and(|length| index >= length) {
            return Lookup::Absent;
        }
        if let Some(row) = self.rows.get(&index) {
            return Lookup::Ready(row);
        }
        match self.blocks.get(&RowRange::block(index).start) {
            Some(Block::Unavailable) => Lookup::Unavailable,
            _ => Lookup::Pending,
        }
    }

    /// Whether a pending block has missed its soft deadline, so the renderer
    /// can show that it is waiting rather than that nothing is happening.
    #[must_use]
    pub fn is_slow(&self, index: u64) -> bool {
        matches!(
            self.blocks.get(&RowRange::block(index).start),
            Some(Block::Requested { issued, .. }) if self.tick.saturating_sub(*issued) >= Self::SOFT_DEADLINE
        )
    }

    /// Moves the viewport and returns the requests that should now be issued.
    ///
    /// Only the blocks actually covered are requested, and never more than the
    /// in-flight limit, so scrolling a long way costs one request per block
    /// landed on rather than one per block crossed.
    pub fn observe(&mut self, viewport: RowRange, now: u64) -> Vec<RowRequest> {
        self.tick = now;
        self.viewport = viewport;
        self.evict();
        self.request(now)
    }

    /// Re-examines outstanding requests, abandoning or retrying those past the
    /// hard deadline. Returns replacement requests.
    pub fn expire(&mut self, now: u64) -> Vec<RowRequest> {
        self.tick = now;
        let stale: Vec<u64> = self
            .blocks
            .iter()
            .filter_map(|(start, block)| Self::overdue(*block, now).then_some(*start))
            .collect();
        for start in stale {
            self.abandon(start);
        }
        self.request(now)
    }

    /// Accepts a delivered window. A window for a superseded generation is
    /// dropped rather than shown beside current rows.
    pub fn deliver(&mut self, window: &RowWindow) -> bool {
        if window.source != self.source || window.version < self.version {
            return false;
        }
        let Ok(delivered) = u32::try_from(window.rows.len()) else {
            return false;
        };
        if window.range.count > RowRange::BLOCK || delivered > window.range.count {
            return false;
        }
        if window.version > self.version {
            self.discard();
            self.version = window.version;
        }
        let start = window.range.start;
        if !matches!(self.blocks.get(&start), Some(Block::Requested { id, .. }) if *id == window.request) {
            return false;
        }
        for (offset, row) in window.rows.iter().enumerate() {
            self.rows.insert(start + offset as u64, row.clone());
        }
        self.blocks.insert(start, Block::Ready);
        self.recency.insert(start, self.tick);
        // Rows land after the scroll that asked for them, so the bound has to
        // be re-applied here; evicting only on scroll overshoots by a window.
        self.evict();
        true
    }

    /// Drops cached rows. An absent range invalidates the whole source.
    pub fn invalidate(&mut self, version: Version, range: Option<RowRange>) {
        self.version = version.max(self.version);
        let Some(range) = range else {
            self.discard();
            return;
        };
        let mut start = RowRange::block(range.start).start;
        while start < range.end() {
            self.forget(start);
            start += u64::from(RowRange::BLOCK);
        }
    }

    /// Applies a producer-side ordering, which invalidates every cached row.
    pub fn sort(&mut self, sort: Option<Sort>) {
        self.sort = sort;
        self.version = self.version.next();
        self.discard();
    }

    /// Applies a producer-side filter, which invalidates every cached row and
    /// the row count with it.
    pub fn filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.version = self.version.next();
        self.discard();
        self.length = None;
    }

    fn request(&mut self, now: u64) -> Vec<RowRequest> {
        let mut issued = Vec::new();
        for start in self.wanted() {
            if self.in_flight() >= Self::IN_FLIGHT_LIMIT {
                break;
            }
            let attempts = match self.blocks.get(&start) {
                None => 0,
                Some(Block::Missing { attempts }) => *attempts,
                _ => continue,
            };
            issued.push(self.claim(start, attempts, now));
        }
        issued
    }

    /// Blocks covering the viewport and its prefetch margin, nearest first, so
    /// a truncated request set still covers what is on screen.
    fn wanted(&self) -> Vec<u64> {
        let block = u64::from(RowRange::BLOCK);
        let first = RowRange::block(self.viewport.start).start;
        let last = RowRange::block(self.viewport.end().saturating_sub(1)).start;
        let mut wanted: Vec<u64> = (first..=last).step_by(block as usize).collect();
        for step in 1..=Self::PREFETCH_BLOCKS {
            wanted.push(last + step * block);
            if let Some(before) = first.checked_sub(step * block) {
                wanted.push(before);
            }
        }
        wanted.retain(|start| self.length.is_none_or(|length| *start < length));
        wanted
    }

    fn claim(&mut self, start: u64, attempts: u8, now: u64) -> RowRequest {
        let id = RequestId::new(self.next_request);
        self.next_request = self.next_request.saturating_add(1);
        self.blocks.insert(
            start,
            Block::Requested {
                id,
                issued: now,
                attempts: attempts.saturating_add(1),
            },
        );
        RowRequest {
            id,
            source: self.source,
            version: self.version,
            range: RowRange::new(start, RowRange::BLOCK),
            sort: self.sort.clone(),
            filter: self.filter.clone(),
        }
    }

    const fn overdue(block: Block, now: u64) -> bool {
        matches!(block, Block::Requested { issued, .. } if now.saturating_sub(issued) >= Self::HARD_DEADLINE)
    }

    /// Retries a timed-out block, or marks it unavailable once attempts run out.
    fn abandon(&mut self, start: u64) {
        let attempts = match self.blocks.get(&start) {
            Some(Block::Requested { attempts, .. }) => *attempts,
            _ => return,
        };
        let next = if attempts >= Self::ATTEMPT_LIMIT {
            Block::Unavailable
        } else {
            Block::Missing { attempts }
        };
        self.blocks.insert(start, next);
    }

    fn forget(&mut self, start: u64) {
        self.blocks.remove(&start);
        self.recency.remove(&start);
        let end = start + u64::from(RowRange::BLOCK);
        self.rows.retain(|index, _| *index < start || *index >= end);
    }

    fn discard(&mut self) {
        self.rows.clear();
        self.blocks.clear();
        self.recency.clear();
    }

    /// Drops the least recently delivered blocks outside the viewport once the
    /// cache exceeds its bound, so a long scroll cannot grow without limit.
    fn evict(&mut self) {
        while self.rows.len() > Self::CAPACITY {
            let Some(start) = self.coldest() else {
                return;
            };
            self.forget(start);
        }
    }

    fn coldest(&self) -> Option<u64> {
        let block = u64::from(RowRange::BLOCK);
        let kept = self.retained();
        self.recency
            .iter()
            .filter(|(start, _)| **start + block <= kept.start || **start >= kept.end())
            .min_by_key(|(_, seen)| **seen)
            .map(|(start, _)| *start)
    }

    /// The viewport plus its prefetch margin, which eviction never touches.
    fn retained(&self) -> RowRange {
        let margin = Self::PREFETCH_BLOCKS * u64::from(RowRange::BLOCK);
        let start = self.viewport.start.saturating_sub(margin);
        let end = self.viewport.end().saturating_add(margin);
        RowRange::new(start, (end - start).try_into().unwrap_or(u32::MAX))
    }
}
