use axum::Json;
use axum::extract::OriginalUri;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::api::DockerError;

pub(super) const MAXIMUM: (u32, u32) = (1, 43);
pub(super) const MINIMUM: (u32, u32) = (1, 24);

/// Docker's answer to a request that reached no route: an unsupported version
/// prefix is a 400, anything else is the daemon's `page not found` 404.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Unrouted {
    pub(super) status: StatusCode,
    pub(super) message: String,
}

#[hl_design::classify(domain = "docker")]
pub(super) fn unrouted(path: &str) -> Unrouted {
    match version_prefix(path) {
        Some(requested) if requested > MAXIMUM => Unrouted {
            status: StatusCode::BAD_REQUEST,
            message: format!(
                "client version {}.{} is too new. Maximum supported API version is {}.{}",
                requested.0, requested.1, MAXIMUM.0, MAXIMUM.1
            ),
        },
        Some(requested) if requested < MINIMUM => Unrouted {
            status: StatusCode::BAD_REQUEST,
            message: format!(
                "client version {}.{} is too old. Minimum supported API version is {}.{}, please upgrade your client to a newer version",
                requested.0, requested.1, MINIMUM.0, MINIMUM.1
            ),
        },
        _ => Unrouted {
            status: StatusCode::NOT_FOUND,
            message: "page not found".into(),
        },
    }
}

/// Docker routes versioned requests through `/v{major}.{minor}/`; any other
/// leading segment is an ordinary path.
#[hl_design::classify(domain = "docker")]
fn version_prefix(path: &str) -> Option<(u32, u32)> {
    let segment = path.strip_prefix('/')?;
    let segment = segment.split('/').next()?;
    let (major, minor) = segment.strip_prefix('v')?.split_once('.')?;
    if major.is_empty() || minor.is_empty() {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Docker's `page not found` body, used wherever a request reaches the daemon
/// but names no resource.
#[hl_design::adapter]
pub(super) fn page_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(DockerError {
            message: "page not found".into(),
        }),
    )
}

#[hl_design::adapter]
pub(super) async fn fallback(OriginalUri(uri): OriginalUri) -> impl IntoResponse {
    let answer = unrouted(uri.path());
    (
        answer.status,
        Json(DockerError {
            message: answer.message,
        }),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    #[test]
    fn unversioned_and_supported_paths_report_docker_page_not_found() {
        for path in [
            "/nonsense",
            "/v1.43/nonsense",
            "/v1.24/nonsense",
            "/vlatest/x",
            "/v1./x",
            "/v.1/x",
        ] {
            let answer = super::unrouted(path);
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(answer.message, "page not found", "{path}");
        }
    }

    #[test]
    fn newer_client_versions_are_refused_with_the_daemon_maximum() {
        for path in ["/v1.44/containers/json", "/v2.0/info"] {
            let answer = super::unrouted(path);
            assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{path}");
            assert!(
                answer
                    .message
                    .ends_with("is too new. Maximum supported API version is 1.43"),
                "{}",
                answer.message
            );
        }
        assert_eq!(
            super::unrouted("/v1.44/containers/json").message,
            "client version 1.44 is too new. Maximum supported API version is 1.43"
        );
    }

    #[test]
    fn older_client_versions_are_refused_with_the_daemon_minimum() {
        assert_eq!(
            super::unrouted("/v1.23/containers/json"),
            super::Unrouted {
                status: StatusCode::BAD_REQUEST,
                message: "client version 1.23 is too old. Minimum supported API version is 1.24, please upgrade your \
                          client to a newer version"
                    .into(),
            }
        );
        assert_eq!(super::unrouted("/v1.9/info").status, StatusCode::BAD_REQUEST);
    }
}
