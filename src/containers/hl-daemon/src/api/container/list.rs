//! `/containers/json` (`docker ps`) list-row DTOs — the container summary and its top-level `Ports[]`
//! entry shape.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

// ---- containers: `docker ps` list rows -------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerSummary {
    pub id: String,
    pub image: String,
    pub command: String,
    pub created: i64,
    pub state: String,
    pub status: String,
    pub exit_code: i64,
    pub ports: Vec<Value>,
    pub labels: HashMap<String, String>,
    pub mounts: Vec<Value>,
    pub names: Vec<String>,
    /// `--size` only: the writable-layer size; omitted otherwise (docker omits the key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_rw: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_root_fs: Option<i64>,
}

// ---- containers: published-port summary (top-level `Ports[]`) --------------
// The top-level `Ports[]` array docker clients read on list/`ps`. Keys aren't plain PascalCase (`IP`),
// so each field carries an explicit rename.

/// One entry of the top-level `Ports` array (`docker ps` / list JSON).
#[derive(Serialize)]
pub(crate) struct PortSummary {
    #[serde(rename = "PublicPort")]
    pub public_port: u16,
    #[serde(rename = "PrivatePort")]
    pub private_port: u16,
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "IP")]
    pub ip: String,
}
