//! Content-addressed snapshot identity and OCI chain-id derivation.

use crate::{Digest, Error, Result};

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Id(String);
impl Id {
    /// # Errors
    /// Returns an error when the identifier is empty or filesystem-unsafe.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        {
            return Err(Error::InvalidMetadata("invalid snapshot id".into()));
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn chain(parent: Option<&Self>, diff: &Digest) -> Result<Self> {
        let chain = match parent {
            None => diff.clone(),
            Some(parent) => {
                let encoded = parent.as_str().strip_prefix("chain-").ok_or_else(|| {
                    Error::InvalidMetadata(format!("snapshot {} is not a layer chain", parent.as_str()))
                })?;
                let parent: Digest = format!("sha256:{encoded}").parse()?;
                Digest::sha256(format!("{parent} {diff}").as_bytes())
            }
        };
        Self::new(format!("chain-{}", chain.encoded()))
    }

    pub(crate) fn chain_digest(&self) -> Result<Digest> {
        let encoded = self
            .as_str()
            .strip_prefix("chain-")
            .ok_or_else(|| Error::InvalidMetadata(format!("snapshot {} is not a layer chain", self.as_str())))?;
        format!("sha256:{encoded}").parse()
    }
}

#[cfg(test)]
mod id_tests {
    use super::Id;
    use crate::Digest;

    #[test]
    fn layer_chain_matches_oci_identity_chain_id() {
        let first_diff = Digest::sha256(b"first layer tar");
        let second_diff = Digest::sha256(b"second layer tar");

        let first = Id::chain(None, &first_diff).unwrap();
        assert_eq!(first.as_str(), format!("chain-{}", first_diff.encoded()));

        let second = Id::chain(Some(&first), &second_diff).unwrap();
        let expected = Digest::sha256(format!("{first_diff} {second_diff}").as_bytes());
        assert_eq!(second.as_str(), format!("chain-{}", expected.encoded()));
    }

    #[test]
    fn layer_chain_rejects_non_chain_parent() {
        let parent = Id::new("container-root").unwrap();
        let diff = Digest::sha256(b"layer tar");

        assert!(Id::chain(Some(&parent), &diff).is_err());
    }
}
