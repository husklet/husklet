//! A parsed image reference: `[registry/]repository[:tag]`, plus the host/tag parse helpers.

use super::*;

/// A parsed image reference: `[registry/]repository[:tag]`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub registry: String, // host[:port], e.g. "registry-1.docker.io", "ghcr.io", "localhost:5000"
    pub repository: String, // path, e.g. "library/ubuntu", "owner/app"
    pub tag: String,
}
impl ImageRef {
    /// Parse a reference the way docker does: the leading segment is a registry host if it contains a
    /// `.` or `:` or is `localhost`; otherwise it's a Docker Hub image (and a single-element repository
    /// gets the implicit `library/` namespace).
    pub fn parse(s: &str) -> ImageRef {
        let (path, tag) = split_tag(s.trim());
        match path.split_once('/') {
            Some((host, rest)) if is_registry_host(host) => {
                let registry = if host == "docker.io" {
                    DOCKER_HUB.to_string()
                } else {
                    host.to_string()
                };
                ImageRef {
                    registry,
                    repository: rest.to_string(),
                    tag,
                }
            }
            _ => {
                let repository = if path.contains('/') {
                    path.to_string()
                } else {
                    format!("library/{path}")
                };
                ImageRef {
                    registry: DOCKER_HUB.to_string(),
                    repository,
                    tag,
                }
            }
        }
    }
    /// `registry/repository:tag`, with Docker Hub abbreviated back to `docker.io`.
    pub fn canonical(&self) -> String {
        let host = if self.registry == DOCKER_HUB {
            "docker.io"
        } else {
            &self.registry
        };
        format!("{host}/{}:{}", self.repository, self.tag)
    }
    /// The short, docker-style display name (`busybox:latest`, `user/app:1`, `ghcr.io/o/a:v2`): Hub's
    /// implicit `docker.io/library/` is elided, other registries are shown.
    pub fn short(&self) -> String {
        let repo = if self.registry == DOCKER_HUB {
            self.repository
                .strip_prefix("library/")
                .unwrap_or(&self.repository)
                .to_string()
        } else {
            format!("{}/{}", self.registry, self.repository)
        };
        format!("{repo}:{}", self.tag)
    }
    pub(super) fn base_url(&self) -> String {
        // local dev registries are plain HTTP; everything else is HTTPS
        let scheme = if is_local_registry(&self.registry) {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}/v2/{}", self.registry, self.repository)
    }
}

fn is_registry_host(seg: &str) -> bool {
    seg == "localhost" || seg.contains('.') || seg.contains(':')
}
pub(super) fn is_local_registry(host: &str) -> bool {
    host.starts_with("localhost") || host.starts_with("127.")
}
fn split_tag(s: &str) -> (&str, String) {
    match s.rsplit_once(':') {
        Some((p, t)) if !t.contains('/') => (p, t.to_string()),
        _ => (s, "latest".to_string()),
    }
}
