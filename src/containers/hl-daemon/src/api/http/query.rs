use serde::Deserialize as _;

#[hl_design::classify(domain = "serde")]
pub(super) fn flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(|value| parse_flag(&value))
}

#[hl_design::classify(domain = "serde")]
pub(super) fn optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        Ok(None)
    } else {
        value.parse().map(Some).map_err(serde::de::Error::custom)
    }
}

#[hl_design::classify(domain = "docker")]
/// Docker's `httputils.BoolValue`: only the falsey spellings are false and no value is rejected.
pub(super) fn parse_flag(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "no" | "false" | "none"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn docker_boolean_query_values_are_coerced_and_never_rejected() {
        for raw in ["", " ", "0", "no", "NO", "false", "FaLsE", "none", "NONE", " 0 "] {
            assert!(!super::parse_flag(raw), "{raw:?}");
        }
        for raw in [
            "1", "true", "TrUe", " true ", "2", "-1", "yes", "off", "maybe", "0.0", "null",
        ] {
            assert!(super::parse_flag(raw), "{raw:?}");
        }
    }
}
