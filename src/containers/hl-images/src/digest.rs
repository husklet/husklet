use std::{fmt, str::FromStr};

use sha2::{Digest as _, Sha256};

use crate::{Error, Result};

/// A validated OCI content digest. SHA-256 is intentionally the first supported algorithm.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest(String);

impl Digest {
    #[must_use]
    pub fn sha256(bytes: &[u8]) -> Self {
        Self::from(<[u8; 32]>::from(Sha256::digest(bytes)))
    }

    #[must_use]
    pub fn algorithm(&self) -> &'static str {
        "sha256"
    }
    #[must_use]
    pub fn encoded(&self) -> &str {
        &self.0[7..]
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<[u8; 32]> for Digest {
    fn from(hash: [u8; 32]) -> Self {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(64);
        for byte in hash {
            encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        Self(format!("sha256:{encoded}"))
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let Some(encoded) = value.strip_prefix("sha256:") else {
            return Err(Error::InvalidDigest(value.into()));
        };
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(Error::InvalidDigest(value.into()));
        }
        Ok(Self(value.into()))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl serde::Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        String::deserialize(deserializer)?.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_vectors() {
        assert_eq!(
            super::Digest::sha256(b"").as_str(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            super::Digest::sha256(b"abc").as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
