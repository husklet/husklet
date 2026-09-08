use std::{fmt, str::FromStr};

use crate::{Digest, Error, Result};

/// The image store's adapter over the runtime-neutral canonical reference value.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Reference {
    canonical: hl_image_reference::ImageReference,
    digest: Option<Digest>,
}

impl Reference {
    #[must_use]
    pub fn registry(&self) -> &str {
        self.canonical.registry()
    }

    #[must_use]
    pub fn repository(&self) -> &str {
        self.canonical.repository()
    }

    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.canonical.tag()
    }

    #[must_use]
    pub fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }

    #[must_use]
    pub fn manifest_selector(&self) -> &str {
        self.canonical.manifest_selector()
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
        let canonical = raw.parse::<hl_image_reference::ImageReference>()?;
        // The neutral parser deliberately has no dependency on the registry transport. Assert at
        // this adapter boundary that every accepted value is representable by that transport.
        canonical
            .to_string()
            .parse::<oci_client::Reference>()
            .map_err(|_| Error::InvalidReference(raw.into()))?;
        let digest = canonical.digest().map(str::parse).transpose()?;
        Ok(Self { canonical, digest })
    }
}

impl serde::Serialize for Reference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Reference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        String::deserialize(deserializer)?.parse().map_err(D::Error::custom)
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.canonical.fmt(formatter)
    }
}
