use serde::Deserialize as _;

#[hl_design::classify(domain = "serde")]
pub(super) fn flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    parse_flag(&value).ok_or_else(|| serde::de::Error::custom(format_args!("invalid boolean {value:?}")))
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
pub(super) fn parse_flag(value: &str) -> Option<bool> {
    match value {
        "1" => Some(true),
        "0" | "" => Some(false),
        value if value.eq_ignore_ascii_case("true") => Some(true),
        value if value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn docker_boolean_spellings_are_explicit() {
        for (raw, expected) in [
            ("1", true),
            ("true", true),
            ("TrUe", true),
            ("0", false),
            ("false", false),
            ("FaLsE", false),
        ] {
            assert_eq!(super::parse_flag(raw), Some(expected));
        }
        assert_eq!(super::parse_flag("yes"), None);
    }
}
