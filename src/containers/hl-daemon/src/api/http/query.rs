use serde::Deserialize as _;

pub(super) fn flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    parse_flag(&value).ok_or_else(|| serde::de::Error::custom(format_args!("invalid boolean {value:?}")))
}

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
