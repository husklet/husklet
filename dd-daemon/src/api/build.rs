//! `docker build` / `docker builder prune` / `docker commit` response DTOs.
//!
//! Typed replacements for the inline `serde_json::json!({…})` builders in `build/handler.rs` and
//! `build/prune.rs`. The build progress stream is NDJSON: each `BuildStream`/`BuildAux` is
//! `serde_json::to_string`'d into one line, exactly as the former `json!(…).to_string()` did. Keys are
//! load-bearing (the docker CLL / bollard parse them): the `stream`/`aux` lines are lowercase, and the
//! prune report + commit id use PascalCase (`CachesDeleted`, `SpaceReclaimed`, `Id`); only the nested
//! aux `ID` is not a plain PascalCase of its field, so it carries an explicit `#[serde(rename)]`.

use serde::Serialize;

/// One `POST /build` progress line: `{"stream": "Step 1/5 : …\n"}`. The field name is already the
/// lowercase docker key, so no rename is needed. (Same wire shape as image `LoadResponse`, kept as its
/// own build-domain type.)
#[derive(Serialize)]
pub(crate) struct BuildStream {
    pub stream: String,
}

/// The final `POST /build` line carrying the built image id: `{"aux": {"ID": "sha256:…"}}` — the docker
/// CLI reads it to report the image ID.
#[derive(Serialize)]
pub(crate) struct BuildAux {
    pub aux: BuildAuxId,
}

/// The nested `aux` object; `ID` is fully uppercase, not a plain PascalCase of `id`.
#[derive(Serialize)]
pub(crate) struct BuildAuxId {
    #[serde(rename = "ID")]
    pub id: String,
}

/// `POST /build/prune` (`docker builder prune`) report: the reclaimed cache ids + freed byte count.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct BuildPruneReport {
    pub caches_deleted: Vec<String>,
    pub space_reclaimed: i64,
}

/// `POST /commit` (`docker commit`) success — the new image id: `{"Id": "sha256:…"}`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct CommitResponse {
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_stream_is_lowercase_stream_key() {
        assert_eq!(
            serde_json::to_value(BuildStream {
                stream: "Step 1/2 : FROM x\n".into()
            })
            .unwrap(),
            serde_json::json!({"stream": "Step 1/2 : FROM x\n"})
        );
    }

    #[test]
    fn build_aux_nests_uppercase_id() {
        assert_eq!(
            serde_json::to_value(BuildAux {
                aux: BuildAuxId {
                    id: "sha256:abc".into()
                }
            })
            .unwrap(),
            serde_json::json!({"aux": {"ID": "sha256:abc"}})
        );
    }

    #[test]
    fn build_prune_report_pascal_case_keys() {
        assert_eq!(
            serde_json::to_value(BuildPruneReport {
                caches_deleted: vec!["buildcache:a".into(), "b.pcache".into()],
                space_reclaimed: 4096
            })
            .unwrap(),
            serde_json::json!({"CachesDeleted": ["buildcache:a", "b.pcache"], "SpaceReclaimed": 4096})
        );
    }

    #[test]
    fn build_prune_report_empty_reclaim() {
        // nothing reclaimed: an empty CachesDeleted array + zero bytes.
        assert_eq!(
            serde_json::to_value(BuildPruneReport {
                caches_deleted: vec![],
                space_reclaimed: 0
            })
            .unwrap(),
            serde_json::json!({"CachesDeleted": [], "SpaceReclaimed": 0})
        );
    }

    #[test]
    fn commit_response_id_key() {
        assert_eq!(
            serde_json::to_value(CommitResponse {
                id: "sha256:deadbeef".into()
            })
            .unwrap(),
            serde_json::json!({"Id": "sha256:deadbeef"})
        );
    }
}
