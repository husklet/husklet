use axum::http::StatusCode;
use std::collections::BTreeMap;

use super::{ApiError, ApiResult};

/// Moby-compatible selection for unused network removal.
#[derive(Debug, Default)]
pub(super) struct Filters {
    labels: Vec<String>,
    excluded_labels: Vec<String>,
    until_ms: Option<u64>,
}

impl Filters {
    pub(super) fn parse(raw: Option<&str>) -> ApiResult<Self> {
        let Some(raw) = raw.filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        let values: BTreeMap<String, Values> = serde_json::from_str(raw).map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid network prune filters: {error}"),
            )
        })?;
        if let Some(name) = values
            .keys()
            .find(|name| !matches!(name.as_str(), "label" | "label!" | "until"))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid filter {name:?}"),
            ));
        }

        let labels = enabled(values.get("label"));
        let excluded_labels = enabled(values.get("label!"));
        let until_ms = match values.get("until") {
            None => None,
            Some(values) => match enabled(Some(values)).as_slice() {
                [value] => Some(
                    value
                        .parse::<crate::api::filter::PruneCutoff>()
                        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?
                        .milliseconds(),
                ),
                _ => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "network prune until requires exactly one value",
                    ));
                }
            },
        };
        Ok(Self {
            labels,
            excluded_labels,
            until_ms,
        })
    }

    pub(super) fn matches(&self, network: &hl_container::Network) -> bool {
        self.until_ms.is_none_or(|until| network.created_at_ms <= until)
            && self.labels.iter().all(|value| matches_label(&network.labels, value))
            && (self.excluded_labels.is_empty()
                || !self
                    .excluded_labels
                    .iter()
                    .all(|value| matches_label(&network.labels, value)))
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Values {
    Current(BTreeMap<String, bool>),
    Legacy(Vec<String>),
}

fn enabled(values: Option<&Values>) -> Vec<String> {
    match values {
        None => Vec::new(),
        Some(Values::Current(values)) => values.keys().cloned().collect(),
        Some(Values::Legacy(values)) => values.clone(),
    }
}

fn matches_label(labels: &BTreeMap<String, String>, value: &str) -> bool {
    value.split_once('=').map_or_else(
        || labels.contains_key(value),
        |(name, expected)| labels.get(name).is_some_and(|actual| actual == expected),
    )
}

#[cfg(test)]
mod tests {
    use super::Filters;
    use hl_container::{Network, NetworkDriver};
    use std::collections::BTreeMap;

    fn network(labels: &[(&str, &str)], created_at_ms: u64) -> Network {
        let mut stored = BTreeMap::new();
        for (name, value) in labels {
            stored.insert((*name).to_owned(), (*value).to_owned());
        }
        Network {
            id: "00000000000000000000000000000000".parse().unwrap(),
            name: "candidate".into(),
            driver: NetworkDriver::None,
            subnet: None,
            gateway: None,
            labels: stored,
            endpoints: BTreeMap::new(),
            created_at_ms,
        }
    }

    #[test]
    fn label_sets_and_cutoff_follow_moby() {
        let filters = Filters::parse(Some(r#"{"label":["owner=team","stage"],"until":["2.000"]}"#)).unwrap();
        assert!(filters.matches(&network(&[("owner", "team"), ("stage", "prod")], 2_000)));
        assert!(!filters.matches(&network(&[("owner", "team")], 2_000)));
        assert!(!filters.matches(&network(&[("owner", "team"), ("stage", "prod")], 2_001)));
    }

    #[test]
    fn negated_label_set_excludes_only_complete_matches() {
        let filters = Filters::parse(Some(r#"{"label!":["owner=team","stage=prod"]}"#)).unwrap();
        assert!(!filters.matches(&network(&[("owner", "team"), ("stage", "prod")], 1)));
        assert!(filters.matches(&network(&[("owner", "team")], 1)));
        assert!(filters.matches(&network(&[], 1)));
    }

    #[test]
    fn prune_rejects_list_filters_and_invalid_cutoffs() {
        for filters in [
            r#"{"driver":["bridge"]}"#,
            r#"{"until":[]}"#,
            r#"{"until":["bad"]}"#,
            r#"{"until":["1","2"]}"#,
        ] {
            assert_eq!(
                Filters::parse(Some(filters)).unwrap_err().status,
                axum::http::StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn current_map_values_are_membership_keys() {
        let filters = Filters::parse(Some(r#"{"label":{"owner=team":false}}"#)).unwrap();
        assert!(filters.matches(&network(&[("owner", "team")], 1)));
        assert!(!filters.matches(&network(&[], 1)));
    }
}
