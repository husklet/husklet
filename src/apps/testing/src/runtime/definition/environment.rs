use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EnvironmentEntry {
    name: Vec<u8>,
    value: Vec<u8>,
}

impl EnvironmentEntry {
    pub(crate) fn name(&self) -> &[u8] {
        &self.name
    }

    pub(crate) fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Environment {
    Text(BTreeMap<String, String>),
    Ordered(Vec<EnvironmentPair>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentPair {
    name: EnvironmentValue,
    value: EnvironmentValue,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EnvironmentValue {
    Text(String),
    Bytes { bytes: Vec<u8> },
}

pub(super) fn environment<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<EnvironmentEntry>, D::Error> {
    match Option::<Environment>::deserialize(deserializer)? {
        None => Ok(Vec::new()),
        Some(environment) => environment.into_entries().map_err(serde::de::Error::custom),
    }
}

impl Environment {
    fn into_entries(self) -> Result<Vec<EnvironmentEntry>, &'static str> {
        let values: Vec<EnvironmentEntry> = match self {
            Self::Text(values) => values
                .into_iter()
                .map(|(name, value)| EnvironmentEntry {
                    name: name.into_bytes(),
                    value: value.into_bytes(),
                })
                .collect(),
            Self::Ordered(values) => values
                .into_iter()
                .map(|entry| EnvironmentEntry {
                    name: entry.name.into_bytes(),
                    value: entry.value.into_bytes(),
                })
                .collect(),
        };
        let bytes = values.iter().try_fold(0_usize, |total, entry| {
            total
                .checked_add(entry.name.len())?
                .checked_add(entry.value.len())?
                .checked_add(2)
        });
        let invalid = values.iter().any(|entry| {
            entry.name.is_empty() || entry.name.contains(&b'=') || entry.name.contains(&0) || entry.value.contains(&0)
        });
        if values.len() > 4096 || bytes.is_none_or(|bytes| bytes > 64 * 1024 * 1024) || invalid {
            Err("invalid or oversized process environment")
        } else {
            Ok(values)
        }
    }
}

impl EnvironmentValue {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Text(value) => value.into_bytes(),
            Self::Bytes { bytes } => bytes,
        }
    }
}
