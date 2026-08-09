//! Measurement-only instrumentation for the aarch64 block cache, enabled by
//! pointing `HL_BLOCK_CACHE_STATS` at an output file.
use std::collections::{BTreeMap, HashMap, HashSet};

const WINDOW: u64 = 100_000;

#[derive(Debug)]
pub(super) struct CacheStats {
    path: std::path::PathBuf,
    limit: usize,
    lookups: u64,
    hits: u64,
    compulsory: u64,
    capacity: u64,
    conflict: u64,
    flushes: u64,
    infinite_hits: u64,
    inserts: u64,
    evictions: u64,
    eviction_index: Vec<u32>,
    seen_ever: HashSet<u64>,
    seen_epoch: HashSet<u64>,
    fa: HashMap<u64, u64>,
    fa_order: BTreeMap<u64, u64>,
    tick: u64,
    assoc: Vec<AssocSim>,
    extents: HashMap<u64, u16>,
    window_set: HashSet<u64>,
    window_left: u64,
    windows: u64,
    window_total: u64,
    window_max: usize,
}

/// A set-associative LRU simulation of the same total capacity.
#[derive(Clone, Debug)]
struct AssocSim {
    ways: usize,
    hits: u64,
    sets: Vec<Vec<(u64, u64)>>,
}

impl AssocSim {
    fn new(ways: usize, limit: usize) -> Self {
        Self {
            ways,
            hits: 0,
            sets: vec![Vec::with_capacity(ways); limit / ways],
        }
    }

    fn access(&mut self, address: u64, tick: u64) {
        let index = (address as usize >> 2) & (self.sets.len() - 1);
        let set = &mut self.sets[index];
        if let Some(entry) = set.iter_mut().find(|(stored, _)| *stored == address) {
            entry.1 = tick;
            self.hits += 1;
            return;
        }
        if set.len() == self.ways {
            let victim = set
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, used))| *used)
                .map_or(0, |(position, _)| position);
            set.swap_remove(victim);
        }
        set.push((address, tick));
    }

    fn clear(&mut self) {
        for set in &mut self.sets {
            set.clear();
        }
    }
}

impl CacheStats {
    pub(super) fn enabled(limit: usize) -> Option<Self> {
        let path = std::env::var_os("HL_BLOCK_CACHE_STATS")?;
        Some(Self {
            path: path.into(),
            limit,
            lookups: 0,
            hits: 0,
            compulsory: 0,
            capacity: 0,
            conflict: 0,
            flushes: 0,
            infinite_hits: 0,
            inserts: 0,
            evictions: 0,
            eviction_index: vec![0; limit],
            seen_ever: HashSet::new(),
            seen_epoch: HashSet::new(),
            fa: HashMap::new(),
            fa_order: BTreeMap::new(),
            tick: 0,
            assoc: vec![
                AssocSim::new(2, limit),
                AssocSim::new(4, limit),
                AssocSim::new(8, limit),
            ],
            extents: HashMap::new(),
            window_set: HashSet::new(),
            window_left: WINDOW,
            windows: 0,
            window_total: 0,
            window_max: 0,
        })
    }

    pub(super) fn lookup(&mut self, address: u64, hit: bool) {
        self.tick += 1;
        self.lookups += 1;
        let tick = self.tick;
        for sim in &mut self.assoc {
            sim.access(address, tick);
        }
        self.window_set.insert(address);
        self.window_left -= 1;
        if self.window_left == 0 {
            self.windows += 1;
            self.window_total += self.window_set.len() as u64;
            self.window_max = self.window_max.max(self.window_set.len());
            self.window_set.clear();
            self.window_left = WINDOW;
        }
        if self.seen_ever.contains(&address) {
            self.infinite_hits += 1;
        } else {
            self.seen_ever.insert(address);
        }
        // Fully-associative LRU of the same capacity: a miss here is a true
        // capacity (or compulsory) miss; a miss only in the real cache is conflict.
        let full_hit = if let Some(used) = self.fa.get_mut(&address) {
            self.fa_order.remove(used);
            *used = tick;
            self.fa_order.insert(tick, address);
            true
        } else {
            if self.fa.len() == self.limit
                && let Some((&oldest, &victim)) = self.fa_order.iter().next()
            {
                self.fa_order.remove(&oldest);
                self.fa.remove(&victim);
            }
            self.fa.insert(address, tick);
            self.fa_order.insert(tick, address);
            false
        };
        if hit {
            self.hits += 1;
            return;
        }
        if !self.seen_epoch.contains(&address) {
            self.seen_epoch.insert(address);
            self.compulsory += 1;
        } else if full_hit {
            self.conflict += 1;
        } else {
            self.capacity += 1;
        }
    }

    pub(super) fn insert(&mut self, address: u64, index: usize, occupied: Option<u64>, instructions: usize) {
        self.inserts += 1;
        self.extents.insert(address, instructions as u16);
        if occupied.is_some_and(|stored| stored != address) {
            self.evictions += 1;
            self.eviction_index[index] = self.eviction_index[index].saturating_add(1);
        }
    }

    pub(super) fn flush(&mut self) {
        self.flushes += 1;
        self.seen_epoch.clear();
        self.fa.clear();
        self.fa_order.clear();
        for sim in &mut self.assoc {
            sim.clear();
        }
    }

    fn report(&self) -> String {
        let mut coverage: HashMap<u64, u32> = HashMap::new();
        for (&start, &length) in &self.extents {
            for step in 0..u64::from(length) {
                *coverage.entry(start + step * 4).or_default() += 1;
            }
        }
        let duplicated: u64 = coverage.values().filter(|count| **count > 1).count() as u64;
        let redundant: u64 = coverage.values().map(|count| u64::from(*count) - 1).sum();
        let mut histogram: Vec<u32> = self.eviction_index.clone();
        histogram.sort_unstable_by(|a, b| b.cmp(a));
        let total: u64 = histogram.iter().map(|count| u64::from(*count)).sum();
        let hot: u64 = histogram
            .iter()
            .take(self.limit / 64)
            .map(|count| u64::from(*count))
            .sum();
        let touched = histogram.iter().filter(|count| **count > 0).count();
        let ratio = |value: u64| {
            if self.lookups == 0 {
                0.0
            } else {
                value as f64 * 100.0 / self.lookups as f64
            }
        };
        format!(
            "block-cache-stats lookups={} hits={} hit_rate={:.3}% \
             compulsory={} ({:.3}%) conflict={} ({:.3}%) capacity={} ({:.3}%) \
             flushes={} inserts={} evictions={} \
             infinite_hit_rate={:.3}% assoc2_hit_rate={:.3}% assoc4_hit_rate={:.3}% assoc8_hit_rate={:.3}% \
             evict_indices_touched={}/{} evict_top_1.5%_share={:.1}% evict_total={} \
             distinct_blocks={} distinct_addresses={} duplicated_instruction_words={} redundant_decodes={} \
             windows={} mean_working_set={:.0} max_working_set={} capacity_limit={}\n",
            self.lookups,
            self.hits,
            ratio(self.hits),
            self.compulsory,
            ratio(self.compulsory),
            self.conflict,
            ratio(self.conflict),
            self.capacity,
            ratio(self.capacity),
            self.flushes,
            self.inserts,
            self.evictions,
            ratio(self.infinite_hits),
            ratio(self.assoc[0].hits),
            ratio(self.assoc[1].hits),
            ratio(self.assoc[2].hits),
            touched,
            self.limit,
            if total == 0 {
                0.0
            } else {
                hot as f64 * 100.0 / total as f64
            },
            total,
            self.extents.len(),
            self.seen_ever.len(),
            duplicated,
            redundant,
            self.windows,
            if self.windows == 0 {
                0.0
            } else {
                self.window_total as f64 / self.windows as f64
            },
            self.window_max,
            self.limit,
        )
    }
}

impl Drop for CacheStats {
    fn drop(&mut self) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = file.write_all(self.report().as_bytes());
        }
    }
}
