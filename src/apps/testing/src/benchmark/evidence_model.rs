use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::benchmark) struct Phase {
    pub us: u64,
    pub ok: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(in crate::benchmark) struct HostLoad {
    pub before: f64,
    pub after: f64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::benchmark) struct Row {
    pub key: String,
    pub workload: String,
    pub layout: String,
    pub cell: String,
    pub round: u32,
    pub position: usize,
    pub arm: String,
    pub output: String,
    pub output_frame: String,
    #[serde(default)]
    pub diagnostic: Option<String>,
    pub phases: BTreeMap<String, Phase>,
    pub host_load: Vec<HostLoad>,
}

impl Row {
    pub fn host_load_valid(&self, samples: u32) -> bool {
        self.host_load.len() == samples as usize
            && self
                .host_load
                .iter()
                .flat_map(|load| [load.before, load.after])
                .all(|load| load.is_finite() && load >= 0.0)
    }

    pub fn host_load_rows(&self) -> impl Iterator<Item = String> + '_ {
        self.host_load
            .iter()
            .enumerate()
            .map(|(repetition, load)| format!("{}\t{repetition}\t{}\t{}", self.key, load.before, load.after))
    }
}
