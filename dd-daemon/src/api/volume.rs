//! `/volumes` DTOs.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

// ---- volumes ---------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeJson {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created_at: String,
    pub scope: &'static str,
    pub labels: HashMap<String, String>,
    pub options: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeList {
    pub volumes: Vec<VolumeJson>,
    pub warnings: Vec<Value>,
}

/// `POST /volumes/prune` report — the names of removed volumes plus reclaimed bytes (always 0; dd
/// does not size volume contents).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumesPruneReport {
    pub volumes_deleted: Vec<String>,
    pub space_reclaimed: i64,
}
