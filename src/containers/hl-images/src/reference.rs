use std::{fmt, str::FromStr};

use crate::{Digest, Error, Result};

const DOCKER_REGISTRY: &str = "registry-1.docker.io";

/// A normalized registry/repository reference with a tag or immutable digest.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Reference {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

impl Reference {
    #[must_use]
    pub fn registry(&self) -> &str {
        &self.registry
    }
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
    #[must_use]
    pub fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }
    pub fn manifest_reference(&self) -> &str {
        self.digest
            .as_ref()
            .map_or_else(|| self.tag.as_deref().unwrap_or("latest"), Digest::as_str)
    }

    pub(crate) fn remote(&self) -> Result<oci_client::Reference> {
        self.to_string()
            .parse()
            .map_err(|error| Error::InvalidReference(format!("{error}")))
    }
}

impl FromStr for Reference {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.chars().any(char::is_whitespace) {
            return Err(Error::InvalidReference(raw.into()));
        }
        let (name, digest) = match raw.split_once('@') {
            Some((name, digest))
                if !name.is_empty() && !digest.is_empty() && !digest.contains('@') =>
            {
                (name, Some(digest.parse()?))
            }
            Some(_) => return Err(Error::InvalidReference(raw.into())),
            None => (raw, None),
        };
        let last_slash = name.rfind('/');
        let last_colon = name.rfind(':');
        let (path, tag) =
            if last_colon.is_some_and(|colon| last_slash.is_none_or(|slash| colon > slash)) {
                let colon = last_colon.expect("checked");
                let tag = &name[colon + 1..];
                if tag.is_empty() {
                    return Err(Error::InvalidReference(raw.into()));
                }
                (&name[..colon], Some(tag.to_owned()))
            } else {
                (name, (digest.is_none()).then(|| "latest".to_owned()))
            };
        let (registry, repository) = match path.split_once('/') {
            Some((first, rest))
                if first == "localhost" || first.contains('.') || first.contains(':') =>
            {
                (
                    if first == "docker.io" {
                        DOCKER_REGISTRY
                    } else {
                        first
                    },
                    rest.to_owned(),
                )
            }
            _ => (
                DOCKER_REGISTRY,
                if path.contains('/') {
                    path.to_owned()
                } else {
                    format!("library/{path}")
                },
            ),
        };
        if registry.is_empty()
            || registry == "."
            || registry == ".."
            || registry.contains('@')
            || repository.is_empty()
            || repository
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(Error::InvalidReference(raw.into()));
        }
        let reference = Self {
            registry: registry.into(),
            repository,
            tag,
            digest,
        };
        reference
            .to_string()
            .parse::<oci_client::Reference>()
            .map_err(|_| Error::InvalidReference(raw.into()))?;
        Ok(reference)
    }
}

impl serde::Serialize for Reference {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> serde::Deserialize<'de> for Reference {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let registry = if self.registry == DOCKER_REGISTRY {
            "docker.io"
        } else {
            &self.registry
        };
        write!(f, "{registry}/{}", self.repository)?;
        if let Some(tag) = &self.tag {
            write!(f, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")?;
        }
        Ok(())
    }
}
