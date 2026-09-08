//! Canonical, runtime-neutral OCI image-reference parsing and normalization.

use std::{fmt, str::FromStr};

const DOCKER_REGISTRY: &str = "registry-1.docker.io";
const MAX_NAME_BYTES: usize = 255;
const MAX_TAG_BYTES: usize = 128;
const SHA256_DIGEST_BYTES: usize = 71;
const MAX_REFERENCE_BYTES: usize = MAX_NAME_BYTES + 1 + MAX_TAG_BYTES + 1 + SHA256_DIGEST_BYTES;

/// A normalized registry/repository reference with a tag or immutable digest.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ImageReference {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<String>,
}

impl ImageReference {
    /// Registry host used for remote requests.
    #[must_use]
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Repository path within the registry.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Explicit or default tag. Digest-only references have no tag.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// Immutable digest, when supplied.
    #[must_use]
    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    /// Selector sent to the manifest endpoint.
    #[must_use]
    pub fn manifest_selector(&self) -> &str {
        self.digest
            .as_deref()
            .unwrap_or_else(|| self.tag.as_deref().unwrap_or("latest"))
    }
}

impl FromStr for ImageReference {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty()
            || raw.len() > MAX_REFERENCE_BYTES
            || raw
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(Error::InvalidReference(raw.into()));
        }
        let (name, digest) = match raw.split_once('@') {
            Some((name, digest)) if !name.is_empty() && !digest.is_empty() && !digest.contains('@') => {
                validate_digest(digest)?;
                (name, Some(digest.to_owned()))
            }
            Some(_) => return Err(Error::InvalidReference(raw.into())),
            None => (raw, None),
        };
        let last_slash = name.rfind('/');
        let last_colon = name.rfind(':');
        let (path, tag) = if last_colon.is_some_and(|colon| last_slash.is_none_or(|slash| colon > slash)) {
            let colon = last_colon.expect("checked");
            let tag = &name[colon + 1..];
            if tag.is_empty() || tag.len() > MAX_TAG_BYTES || !valid_tag(tag) {
                return Err(Error::InvalidReference(raw.into()));
            }
            (&name[..colon], Some(tag.to_owned()))
        } else {
            (name, (digest.is_none()).then(|| "latest".to_owned()))
        };
        let (registry, mut repository) = match path.split_once('/') {
            Some((first, rest)) if first == "localhost" || first.contains('.') || first.contains(':') => (
                if matches!(first, "docker.io" | DOCKER_REGISTRY) {
                    DOCKER_REGISTRY
                } else {
                    first
                },
                rest.to_owned(),
            ),
            _ => (
                DOCKER_REGISTRY,
                if path.contains('/') {
                    path.to_owned()
                } else {
                    format!("library/{path}")
                },
            ),
        };
        if registry == DOCKER_REGISTRY && !repository.contains('/') {
            repository = format!("library/{repository}");
        }
        if registry.is_empty()
            || registry == "."
            || registry == ".."
            || registry.contains('@')
            || repository.is_empty()
            || repository
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || registry.len() + 1 + repository.len() > MAX_NAME_BYTES
            || !valid_registry(registry)
            || !repository.split('/').all(valid_repository_component)
        {
            return Err(Error::InvalidReference(raw.into()));
        }
        let reference = Self {
            registry: registry.into(),
            repository,
            tag,
            digest,
        };
        Ok(reference)
    }
}

fn valid_registry(value: &str) -> bool {
    // The registry transport currently rejects bracketed IPv6 authorities. Keeping them invalid
    // here prevents a value from passing the shared parser only to fail at the network boundary.
    if value.starts_with('[') {
        return false;
    }
    let (host, port) = value
        .rsplit_once(':')
        .map_or((value, None), |(host, port)| (host, Some(port)));
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        })
        && port.is_none_or(valid_port)
}

fn valid_port(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_repository_component(value: &str) -> bool {
    let mut expecting_alphanumeric = true;
    let mut bytes = value.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            expecting_alphanumeric = false;
            continue;
        }
        if expecting_alphanumeric || !matches!(byte, b'.' | b'_' | b'-') {
            return false;
        }
        if byte == b'_' && bytes.peek() == Some(&b'_') {
            bytes.next();
        } else if byte == b'-' {
            while bytes.peek() == Some(&b'-') {
                bytes.next();
            }
        }
        expecting_alphanumeric = true;
    }
    !expecting_alphanumeric
}

fn valid_tag(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && value
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn validate_digest(value: &str) -> Result<(), Error> {
    let Some(encoded) = value.strip_prefix("sha256:") else {
        return Err(Error::InvalidDigest(value.into()));
    };
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::InvalidDigest(value.into()));
    }
    Ok(())
}

impl serde::Serialize for ImageReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ImageReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        String::deserialize(deserializer)?.parse().map_err(D::Error::custom)
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let registry = if self.registry == DOCKER_REGISTRY {
            "docker.io"
        } else {
            &self.registry
        };
        write!(formatter, "{registry}/{}", self.repository)?;
        if let Some(tag) = &self.tag {
            write!(formatter, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(formatter, "@{digest}")?;
        }
        Ok(())
    }
}

/// A malformed image reference or digest.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("invalid image reference: {0}")]
    InvalidReference(String),
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
}

/// Compatibility name for consumers whose domain already calls the value a reference.
pub type Reference = ImageReference;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_hub_shorthand_has_one_canonical_identity() {
        for raw in [
            "alpine",
            "library/alpine",
            "docker.io/alpine",
            "docker.io/library/alpine",
            "registry-1.docker.io/alpine",
            "registry-1.docker.io/library/alpine",
        ] {
            let reference: ImageReference = raw.parse().unwrap();
            assert_eq!(reference.registry(), DOCKER_REGISTRY);
            assert_eq!(reference.repository(), "library/alpine");
            assert_eq!(reference.tag(), Some("latest"));
            assert_eq!(reference.to_string(), "docker.io/library/alpine:latest");
        }
    }

    #[test]
    fn tags_digests_and_registry_ports_are_unambiguous() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let tagged: ImageReference = "localhost:5000/team/app:v1".parse().unwrap();
        assert_eq!(tagged.registry(), "localhost:5000");
        assert_eq!(tagged.repository(), "team/app");
        assert_eq!(tagged.tag(), Some("v1"));
        let pinned: ImageReference = format!("ghcr.io/husklet/app:v2@{digest}").parse().unwrap();
        assert_eq!(pinned.tag(), Some("v2"));
        assert_eq!(pinned.digest(), Some(digest.as_str()));
        assert_eq!(pinned.manifest_selector(), digest);
    }

    #[test]
    fn unrelated_registry_hosts_are_never_aliased() {
        for host in ["index.docker.io", "docker.example", "registry-1.docker.io.example"] {
            let reference: ImageReference = format!("{host}/library/alpine").parse().unwrap();
            assert_eq!(reference.registry(), host);
            assert!(reference.to_string().starts_with(host));
        }
    }

    #[test]
    fn whitespace_controls_and_path_ambiguity_are_rejected() {
        for raw in [
            " alpine",
            "alpine ",
            "al\tpine",
            "al\npine",
            "al\0pine",
            "../alpine",
            "host/repository/../../outside",
            "host//repository",
            "host/./repository",
            "/repository",
            "repository/",
            "repository@sha256:bad",
            "repository@@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "[::1]:5000/repository",
            "Example/Repository",
            "example.test/Uppercase",
            "example.test/repository:.tag",
            "example.test/repository:-tag",
            "-example.test/repository",
            "example..test/repository",
        ] {
            assert!(raw.parse::<ImageReference>().is_err(), "accepted {raw:?}");
        }
    }

    #[test]
    fn reference_name_and_tag_bounds_are_enforced() {
        let accepted_name = format!("example.test/{}", "a".repeat(MAX_NAME_BYTES - "example.test/".len()));
        assert!(accepted_name.parse::<ImageReference>().is_ok());
        let oversized_name = format!(
            "example.test/{}",
            "a".repeat(MAX_NAME_BYTES - "example.test/".len() + 1)
        );
        assert!(oversized_name.parse::<ImageReference>().is_err());
        assert!(
            format!("alpine:{}", "a".repeat(MAX_TAG_BYTES))
                .parse::<ImageReference>()
                .is_ok()
        );
        assert!(
            format!("alpine:{}", "a".repeat(MAX_TAG_BYTES + 1))
                .parse::<ImageReference>()
                .is_err()
        );
    }

    #[test]
    fn serde_cannot_bypass_validation() {
        assert!(serde_json::from_str::<ImageReference>(r#"" ../escape""#).is_err());
        let reference: ImageReference = "ghcr.io/husklet/app:v1".parse().unwrap();
        assert_eq!(
            serde_json::from_str::<ImageReference>(&serde_json::to_string(&reference).unwrap()).unwrap(),
            reference
        );
    }
}
