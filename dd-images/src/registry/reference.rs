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
/// Split a reference into `(repository, tag)`, defaulting to `latest`. A `:port` inside a registry host
/// (`localhost:5000/foo`) is NOT a tag — only a final `:tag` with no slash after the colon counts. This
/// is the one implementation of that rule; [`crate::image::ref_tag`] delegates here for the tag alone.
pub(crate) fn split_tag(s: &str) -> (&str, String) {
    match s.rsplit_once(':') {
        Some((p, t)) if !t.contains('/') => (p, t.to_string()),
        _ => (s, "latest".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_single_name_short_and_canonical() {
        let r = ImageRef::parse("ubuntu");
        // Hub's implicit docker.io/library/ is elided in the short display name.
        assert_eq!(r.short(), "ubuntu:latest");
        // canonical() abbreviates the Hub host back to docker.io but keeps library/.
        assert_eq!(r.canonical(), "docker.io/library/ubuntu:latest");
    }

    #[test]
    fn hub_user_repo_keeps_namespace() {
        let r = ImageRef::parse("user/app:1");
        assert_eq!(r.short(), "user/app:1"); // multi-segment repo: nothing stripped
        assert_eq!(r.canonical(), "docker.io/user/app:1");
    }

    #[test]
    fn other_registry_shown_in_short() {
        let r = ImageRef::parse("ghcr.io/o/a:v2");
        assert_eq!(r.short(), "ghcr.io/o/a:v2");
        assert_eq!(r.canonical(), "ghcr.io/o/a:v2");
    }

    #[test]
    fn localhost_registry_with_port() {
        let r = ImageRef::parse("localhost:5000/img");
        // registry host has a port; short()/canonical() show it verbatim, default tag latest.
        assert_eq!(r.short(), "localhost:5000/img:latest");
        assert_eq!(r.canonical(), "localhost:5000/img:latest");
    }
}
