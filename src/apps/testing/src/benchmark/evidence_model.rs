use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::benchmark) struct Phase {
    pub us: u64,
    pub ok: String,
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
    pub phases: BTreeMap<String, Phase>,
    pub host_load: String,
}
