//! A parsed image reference: `[registry/]repository[:tag]`, plus the host/tag parse helpers.

use super::*;
use std::convert::Infallible;
use std::str::FromStr;

/// A parsed image reference: `[registry/]repository[:tag][@digest]`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    /// Registry `host[:port]`, e.g. `"registry-1.docker.io"`, `"ghcr.io"`, `"localhost:5000"`.
    pub registry: String,
    /// Repository path within the registry, e.g. `"library/ubuntu"`, `"owner/app"`.
    pub repository: String,
    /// Image tag, defaulting to `"latest"` when the reference omits one.
    pub tag: String,
    /// A pinned content digest (`sha256:<hex>`) when the reference used the `@digest` form
    /// (`alpine@sha256:…`); manifest requests then target the digest, not the tag. `None` for a plain
    /// tag reference.
    pub digest: Option<String>,
}
impl ImageRef {
    /// Parses a reference and replaces its tag when a non-empty override is supplied.
    pub fn with_tag(source: &str, tag: &str) -> Self {
        let mut reference = Self::parse_source(source);
        if !tag.is_empty() {
            reference.tag = tag.to_owned();
        }
        reference
    }

    /// The final repository path component, without tag or digest.
    pub fn name(&self) -> &str {
        self.repository
            .rsplit('/')
            .next()
            .unwrap_or(&self.repository)
    }

    /// Fully qualified repository identity without tag or digest.
    pub fn repository_identity(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }
    /// Parse a reference the way docker does: the leading segment is a registry host if it contains a
    /// `.` or `:` or is `localhost`; otherwise it's a Docker Hub image (and a single-element repository
    /// gets the implicit `library/` namespace). A trailing `@sha256:<hex>` is a pinned DIGEST — it is
    /// split off first (so it is not mistaken for a `:tag`) and carried in [`digest`](Self::digest).
    fn parse_source(s: &str) -> ImageRef {
        // Split off a `@digest` suffix BEFORE tag parsing, so `alpine@sha256:<hex>` doesn't get its
        // digest colon read as a `:tag` split (which produced repo `library/alpine@sha256`, tag `<hex>`).
        let (name, digest) = match s.trim().split_once('@') {
            Some((n, d)) if !d.is_empty() => (n, Some(d.to_string())),
            _ => (s.trim(), None),
        };
        let (path, tag) = Self::split(name);
        match path.split_once('/') {
            Some((host, rest)) if Self::registry_host(host) => {
                let registry = if host == "docker.io" {
                    DOCKER_HUB.to_string()
                } else {
                    host.to_string()
                };
                ImageRef {
                    registry,
                    repository: rest.to_string(),
                    tag,
                    digest,
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
                    digest,
                }
            }
        }
    }

    /// The manifest reference to request: the pinned `@digest` when present, else the tag. A digest-pinned
    /// pull fetches `/manifests/sha256:<hex>` (exact content), a plain reference `/manifests/<tag>`.
    pub(super) fn manifest_reference(&self) -> &str {
        self.digest.as_deref().unwrap_or(&self.tag)
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
        let scheme = if self.local_registry() {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}/v2/{}", self.registry, self.repository)
    }

    fn registry_host(segment: &str) -> bool {
        segment == "localhost" || segment.contains('.') || segment.contains(':')
    }

    pub(super) fn local_registry(&self) -> bool {
        self.registry.starts_with("localhost") || self.registry.starts_with("127.")
    }

    fn split(source: &str) -> (&str, String) {
        match source.rsplit_once(':') {
            Some((path, tag)) if !tag.contains('/') => (path, tag.to_owned()),
            _ => (source, "latest".to_owned()),
        }
    }

    pub(super) fn resolve(&self, location: &str) -> Result<String, crate::Error> {
        if location.starts_with("https://") || location.starts_with("http://") {
            return Ok(location.to_owned());
        }
        if !location.starts_with('/') {
            return Err(crate::Error::Registry(format!(
                "invalid registry Location: {location}"
            )));
        }
        let base = self.base_url();
        let origin = base
            .split("/v2/")
            .next()
            .ok_or_else(|| crate::Error::Registry(format!("invalid registry base URL: {base}")))?;
        Ok(format!("{origin}{location}"))
    }
}

impl FromStr for ImageRef {
    type Err = Infallible;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_source(source))
    }
}

impl From<&str> for ImageRef {
    fn from(source: &str) -> Self {
        source.parse().unwrap_or_else(|never| match never {})
    }
}

impl From<&String> for ImageRef {
    fn from(source: &String) -> Self {
        Self::from(source.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_single_name_short_and_canonical() {
        let r = ImageRef::from("ubuntu");
        // Hub's implicit docker.io/library/ is elided in the short display name.
        assert_eq!(r.short(), "ubuntu:latest");
        // canonical() abbreviates the Hub host back to docker.io but keeps library/.
        assert_eq!(r.canonical(), "docker.io/library/ubuntu:latest");
    }

    #[test]
    fn hub_user_repo_keeps_namespace() {
        let r = ImageRef::from("user/app:1");
        assert_eq!(r.short(), "user/app:1"); // multi-segment repo: nothing stripped
        assert_eq!(r.canonical(), "docker.io/user/app:1");
    }

    #[test]
    fn other_registry_shown_in_short() {
        let r = ImageRef::from("ghcr.io/o/a:v2");
        assert_eq!(r.short(), "ghcr.io/o/a:v2");
        assert_eq!(r.canonical(), "ghcr.io/o/a:v2");
    }

    #[test]
    fn localhost_registry_with_port() {
        let r = ImageRef::from("localhost:5000/img");
        // registry host has a port; short()/canonical() show it verbatim, default tag latest.
        assert_eq!(r.short(), "localhost:5000/img:latest");
        assert_eq!(r.canonical(), "localhost:5000/img:latest");
    }

    // Finding 9: a `@sha256:<hex>` digest-pinned reference must parse to the repository WITHOUT the
    // `@sha256` suffix and carry the digest, so the pull requests `/manifests/sha256:<hex>`.
    #[test]
    fn digest_pinned_reference_parses_repository_and_digest() {
        let hex = "a".repeat(64);
        let r = ImageRef::from(&format!("alpine@sha256:{hex}"));
        // canonical Hub namespacing, and NO `@sha256` bleed into the repository.
        assert_eq!(r.repository, "library/alpine");
        assert!(
            !r.repository.contains('@'),
            "repo must not carry the digest"
        );
        assert_eq!(r.digest, Some(format!("sha256:{hex}")));
        // manifest requests target the digest, not the (defaulted) tag.
        assert_eq!(r.manifest_reference(), format!("sha256:{hex}"));
        assert_eq!(r.tag, "latest");
    }

    #[test]
    fn digest_pinned_reference_with_registry_and_tag() {
        let hex = "b".repeat(64);
        // registry + user repo + explicit tag + digest all together.
        let r = ImageRef::from(&format!("ghcr.io/o/a:v2@sha256:{hex}"));
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "o/a");
        assert_eq!(r.tag, "v2");
        assert_eq!(r.digest, Some(format!("sha256:{hex}")));
        assert_eq!(r.manifest_reference(), format!("sha256:{hex}"));
    }

    #[test]
    fn plain_reference_has_no_digest_and_uses_tag() {
        let r = ImageRef::from("alpine:3.19");
        assert_eq!(r.digest, None);
        assert_eq!(r.manifest_reference(), "3.19");
    }

    // ---- split_tag: the single source of the "final :tag with no slash after it" rule ----
    #[test]
    fn split_tag_explicit_tag() {
        assert_eq!(
            ImageRef::split("ubuntu:24.04"),
            ("ubuntu", "24.04".to_string())
        );
    }
    #[test]
    fn split_tag_defaults_latest_when_absent() {
        assert_eq!(ImageRef::split("ubuntu"), ("ubuntu", "latest".to_string()));
    }
    #[test]
    fn split_tag_registry_port_is_not_a_tag() {
        // The final colon's right side contains a '/', so it is a host:port, not a tag -> default latest,
        // repository left whole.
        assert_eq!(
            ImageRef::split("localhost:5000/foo"),
            ("localhost:5000/foo", "latest".to_string())
        );
        // ...but a real tag AFTER the port path is detected.
        assert_eq!(
            ImageRef::split("localhost:5000/foo:1.2"),
            ("localhost:5000/foo", "1.2".to_string())
        );
    }
    #[test]
    fn split_tag_rightmost_colon_wins() {
        // rsplit_once splits at the LAST colon; earlier colons stay in the repository part.
        assert_eq!(ImageRef::split("a:b:c"), ("a:b", "c".to_string()));
    }
    #[test]
    fn split_tag_leading_colon_empty_repo() {
        // A leading ':tag' leaves an empty repository and the literal tag (no slash after the colon).
        assert_eq!(ImageRef::split(":tag"), ("", "tag".to_string()));
    }
    #[test]
    fn split_tag_empty_and_trailing_colon() {
        assert_eq!(ImageRef::split(""), ("", "latest".to_string()));
        // A trailing colon -> empty tag string (there is no '/' after it), NOT the "latest" default.
        assert_eq!(ImageRef::split("ubuntu:"), ("ubuntu", "".to_string()));
    }

    // ---- is_local_registry: plain-HTTP hosts (localhost / 127.x) ----
    #[test]
    fn is_local_registry_true_cases() {
        assert!(ImageRef::from("localhost/image").local_registry());
        assert!(ImageRef::from("localhost:5000/image").local_registry());
        assert!(ImageRef::from("127.0.0.1/image").local_registry());
        assert!(ImageRef::from("127.0.0.1:5000/image").local_registry());
    }
    #[test]
    fn is_local_registry_false_cases() {
        assert!(!ImageRef::from("registry-1.docker.io").local_registry());
        assert!(!ImageRef::from("ghcr.io").local_registry());
        // "127" without the trailing dot does NOT match the "127." prefix (characterization of the exact rule).
        assert!(!ImageRef::from("127examples.io").local_registry());
    }

    // ---- base_url: http for local dev registries, https otherwise; /v2/<repository> path ----
    #[test]
    fn base_url_local_registry_is_http() {
        let r = ImageRef::from("localhost:5000/img");
        assert_eq!(r.base_url(), "http://localhost:5000/v2/img");
    }
    #[test]
    fn base_url_remote_registry_is_https() {
        let r = ImageRef::from("ghcr.io/o/a:v2");
        assert_eq!(r.base_url(), "https://ghcr.io/v2/o/a");
    }

    #[test]
    fn resolves_absolute_and_root_relative_locations() {
        let reference = ImageRef::from("ghcr.io/o/a:v2");
        assert_eq!(
            reference.resolve("https://cdn.example/blob").unwrap(),
            "https://cdn.example/blob"
        );
        assert_eq!(
            reference.resolve("/v2/o/a/blobs/x").unwrap(),
            "https://ghcr.io/v2/o/a/blobs/x"
        );
        assert!(reference.resolve("relative/path").is_err());
    }

    #[test]
    fn from_str_uses_reference_parser() {
        let reference: ImageRef = "localhost:5000/app:1".parse().unwrap();
        assert_eq!(reference.registry, "localhost:5000");
        assert_eq!(reference.repository, "app");
        assert_eq!(reference.tag, "1");
    }
    #[test]
    fn base_url_hub_uses_full_host_and_library_namespace() {
        // A bare Hub name expands to the registry-1.docker.io host + library/ namespace in the v2 URL.
        let r = ImageRef::from("ubuntu");
        assert_eq!(
            r.base_url(),
            "https://registry-1.docker.io/v2/library/ubuntu"
        );
    }
}
