//! `/exec` DTOs.

use serde::Serialize;

// ---- exec ------------------------------------------------------------------

/// `POST /containers/{id}/exec` ack — `{"Id": <exec id>}`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ExecCreateResponse {
    pub id: String,
}

/// `GET /exec/{id}/json` (`docker exec` inspect). `ID`/`ContainerID` need explicit renames (PascalCase
/// would drop the capitalization).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ExecInspect {
    #[serde(rename = "ID")]
    pub id: String,
    pub running: bool,
    pub exit_code: i64,
    #[serde(rename = "ContainerID")]
    pub container_id: String,
    pub process_config: ExecProcessConfig,
}

/// The nested `ProcessConfig` — docker's lowercase keys verbatim (no PascalCase).
#[derive(Serialize)]
pub(crate) struct ExecProcessConfig {
    pub tty: bool,
    pub privileged: bool,
    pub entrypoint: String,
    pub arguments: Vec<String>,
}
