//! Small `/containers` ack / report DTOs — `top`, create ack, prune / update acks, and `diff` changes.

use serde::Serialize;
use serde_json::Value;

// ---- containers: `docker top` ----------------------------------------------

/// `GET /containers/{id}/top` — a single synthetic process row (dd has no guest process tree).
#[derive(Serialize)]
pub(crate) struct ContainerTop {
    #[serde(rename = "Titles")]
    pub titles: Vec<&'static str>,
    #[serde(rename = "Processes")]
    pub processes: Vec<Vec<String>>,
}

// ---- containers: create ack ------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct CreateResponse {
    pub id: String,
    pub warnings: Vec<Value>,
}

// ---- containers: prune / update acks ---------------------------------------

/// `POST /containers/prune` report — the ids of the removed (exited) containers plus reclaimed bytes
/// (always 0; dd does not size container writable layers).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainersPruneReport {
    pub containers_deleted: Vec<String>,
    pub space_reclaimed: i64,
}

/// `POST /containers/{id}/update` ack — `{"Warnings": []}`. dd applies no live resource limits, so the
/// envelope is always empty.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerUpdateResponse {
    pub warnings: Vec<Value>,
}

// ---- containers: `docker diff` changes -------------------------------------

/// One entry of the `GET /containers/{id}/changes` (`docker diff`) array — a changed container-absolute
/// path and its kind (`0`=Modified, `1`=Added, `2`=Deleted).
#[derive(Serialize)]
pub(crate) struct ContainerChange {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Kind")]
    pub kind: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_prune_report_shape() {
        // Populated case.
        assert_eq!(
            serde_json::to_value(ContainersPruneReport {
                containers_deleted: vec!["c1".into(), "c2".into()],
                space_reclaimed: 0,
            })
            .unwrap(),
            serde_json::json!({"ContainersDeleted": ["c1", "c2"], "SpaceReclaimed": 0})
        );
        // Empty case — the deleted list stays an empty array (never null).
        assert_eq!(
            serde_json::to_value(ContainersPruneReport {
                containers_deleted: vec![],
                space_reclaimed: 0,
            })
            .unwrap(),
            serde_json::json!({"ContainersDeleted": [], "SpaceReclaimed": 0})
        );
    }

    #[test]
    fn container_update_response_shape() {
        assert_eq!(
            serde_json::to_value(ContainerUpdateResponse { warnings: vec![] }).unwrap(),
            serde_json::json!({"Warnings": []})
        );
    }

    #[test]
    fn container_change_shape() {
        assert_eq!(
            serde_json::to_value(ContainerChange {
                path: "/etc/hosts".into(),
                kind: 0,
            })
            .unwrap(),
            serde_json::json!({"Path": "/etc/hosts", "Kind": 0})
        );
        // The Kind is a bare number for every kind (added/deleted included).
        assert_eq!(
            serde_json::to_value(ContainerChange {
                path: "/tmp/new".into(),
                kind: 2,
            })
            .unwrap(),
            serde_json::json!({"Path": "/tmp/new", "Kind": 2})
        );
    }
}
