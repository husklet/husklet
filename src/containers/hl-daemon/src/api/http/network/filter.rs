use axum::http::StatusCode;
use hl_container::NetworkDriver;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

use super::{ApiError, ApiResult};

#[derive(Default)]
pub(super) struct ListFilters {
    dangling: Option<bool>,
    driver: Vec<String>,
    id: Vec<Pattern>,
    label: Vec<String>,
    name: Vec<Pattern>,
    scope: Vec<String>,
    network_type: Vec<NetworkType>,
}

#[derive(Default)]
pub(super) struct Filters(BTreeMap<String, Vec<String>>);

impl Filters {
    pub(super) fn parse(raw: Option<String>) -> ApiResult<Self> {
        let Some(raw) = raw.filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        let values: BTreeMap<String, serde_json::Value> = serde_json::from_str(&raw)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        let mut filters = BTreeMap::new();
        for (name, values) in values {
            if !matches!(name.as_str(), "label" | "label!" | "until") {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unsupported network filter {name:?}"),
                ));
            }
            let values = match values {
                serde_json::Value::Array(values) => values
                    .into_iter()
                    .map(|value| {
                        value.as_str().map(str::to_owned).ok_or_else(|| {
                            ApiError::new(StatusCode::BAD_REQUEST, "filter values must be strings")
                        })
                    })
                    .collect::<ApiResult<Vec<_>>>()?,
                serde_json::Value::Object(values) => values
                    .into_iter()
                    .filter_map(|(value, enabled)| (enabled == true).then_some(value))
                    .collect(),
                _ => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "filter values must be arrays or objects",
                    ));
                }
            };
            filters.insert(name, values);
        }
        Ok(Self(filters))
    }

    pub(super) fn matches(&self, network: &hl_container::Network) -> bool {
        self.all_values("label!", |value| !matches_label(&network.labels, value))
            && self.values("until", |value| {
                value
                    .parse::<crate::api::filter::PruneCutoff>()
                    .is_ok_and(|until| network.created_at_ms < until.milliseconds())
            })
            && self.values("label", |value| matches_label(&network.labels, value))
    }

    fn values(&self, name: &str, predicate: impl Fn(&String) -> bool) -> bool {
        self.0.get(name).is_none_or(|values| values.iter().any(predicate))
    }

    fn all_values(&self, name: &str, predicate: impl Fn(&String) -> bool) -> bool {
        self.0.get(name).is_none_or(|values| values.iter().all(predicate))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Values {
    Current(BTreeMap<String, bool>),
    Legacy(Vec<String>),
}

impl Values {
    fn terms(self) -> BTreeSet<String> {
        match self {
            // Docker's current filter encoding is a string set represented as an
            // object. Its decoder retains every key; the boolean is not a switch.
            Self::Current(values) => values.into_keys().collect(),
            Self::Legacy(values) => values.into_iter().collect(),
        }
    }
}

struct Pattern {
    exact: String,
    regex: Option<Regex>,
}

impl Pattern {
    fn new(value: String) -> Self {
        let regex = Regex::new(&value).ok();
        Self { exact: value, regex }
    }

    fn matches(&self, value: &str) -> bool {
        self.exact == value || self.regex.as_ref().is_some_and(|pattern| pattern.is_match(value))
    }
}

enum NetworkType {
    Builtin,
    Custom,
}

impl ListFilters {
    pub(super) fn parse(raw: Option<&str>) -> ApiResult<Self> {
        let Some(raw) = raw.filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        let mut values: BTreeMap<String, Values> = serde_json::from_str(raw).map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid network list filters: {error}"),
            )
        })?;
        if let Some(name) = values.keys().find(|name| {
            !matches!(
                name.as_str(),
                "dangling" | "driver" | "id" | "label" | "name" | "scope" | "type"
            )
        }) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid filter {name:?}"),
            ));
        }

        let dangling = match take(&mut values, "dangling").as_slice() {
            [] => None,
            [value] if matches!(value.as_str(), "1" | "true") => Some(true),
            [value] if matches!(value.as_str(), "0" | "false") => Some(false),
            [_] => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid value for filter 'dangling'",
                ));
            }
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "got more than one value for filter key \"dangling\"",
                ));
            }
        };
        let network_type = take(&mut values, "type")
            .into_iter()
            .map(|value| match value.as_str() {
                "builtin" => Ok(NetworkType::Builtin),
                "custom" => Ok(NetworkType::Custom),
                _ => Err(ApiError::new(
                    // Moby's type validator returns an unclassified error, which its HTTP mapper exposes as 500.
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("invalid filter: 'type'='{value}'"),
                )),
            })
            .collect::<ApiResult<_>>()?;
        Ok(Self {
            dangling,
            driver: take(&mut values, "driver"),
            id: take(&mut values, "id").into_iter().map(Pattern::new).collect(),
            label: take(&mut values, "label"),
            name: take(&mut values, "name").into_iter().map(Pattern::new).collect(),
            scope: take(&mut values, "scope"),
            network_type,
        })
    }

    pub(super) fn matches(&self, network: &hl_container::Network) -> bool {
        let driver = match network.driver {
            NetworkDriver::None => "null",
            NetworkDriver::Bridge => "bridge",
        };
        matches_exact(&self.driver, driver)
            && matches_patterns(&self.id, network.id.as_str())
            && matches_patterns(&self.name, &network.name)
            && matches_exact(&self.scope, "local")
            && self.label.iter().all(|value| matches_label(&network.labels, value))
            && self.dangling.is_none_or(|expected| {
                let dangling = !network.predefined() && network.endpoints.is_empty();
                dangling == expected
            })
            && (self.network_type.is_empty()
                || self.network_type.iter().any(|value| match value {
                    NetworkType::Builtin => network.predefined(),
                    NetworkType::Custom => !network.predefined(),
                }))
    }
}

fn take(values: &mut BTreeMap<String, Values>, name: &str) -> Vec<String> {
    values
        .remove(name)
        .map(Values::terms)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn matches_exact(expected: &[String], actual: &str) -> bool {
    expected.is_empty() || expected.iter().any(|value| value == actual)
}

fn matches_patterns(expected: &[Pattern], actual: &str) -> bool {
    expected.is_empty() || expected.iter().any(|value| value.matches(actual))
}

fn matches_label(labels: &BTreeMap<String, String>, value: &str) -> bool {
    value.split_once('=').map_or_else(
        || labels.contains_key(value),
        |(name, expected)| labels.get(name).is_some_and(|actual| actual == expected),
    )
}
