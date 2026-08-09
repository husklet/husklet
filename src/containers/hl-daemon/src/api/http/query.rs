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

/// Docker advertises the multiplexed frame media type from v1.42, and the raw type for a TTY.
#[hl_design::classify(domain = "docker")]
pub(super) fn stream_content_type(uri: &axum::http::Uri, terminal: bool) -> &'static str {
    let supports_multiplexed_type = uri
        .path()
        .strip_prefix("/v1.")
        .and_then(|path| path.split('/').next())
        .and_then(|minor| minor.parse::<u16>().ok())
        .is_none_or(|minor| minor >= 42);
    if supports_multiplexed_type && !terminal {
        "application/vnd.docker.multiplexed-stream"
    } else {
        "application/vnd.docker.raw-stream"
    }
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

    /// Docker 29.1.3 answered `multiplexed-stream` on a non-TTY `GET /v1.44/containers/x/logs`
    /// and `raw-stream` on a TTY container, matching its attach and exec framing.
    #[test]
    fn stream_media_type_follows_the_tty_and_the_negotiated_version() {
        for path in [
            "/containers/id/logs",
            "/v1.42/containers/id/logs",
            "/v1.44/exec/id/start",
        ] {
            let uri = path.parse().unwrap();
            assert_eq!(
                super::stream_content_type(&uri, false),
                "application/vnd.docker.multiplexed-stream",
                "{path}"
            );
            assert_eq!(
                super::stream_content_type(&uri, true),
                "application/vnd.docker.raw-stream",
                "{path}"
            );
        }
        let legacy = "/v1.41/containers/id/logs".parse().unwrap();
        assert_eq!(
            super::stream_content_type(&legacy, false),
            "application/vnd.docker.raw-stream"
        );
    }
}
