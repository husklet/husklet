use axum::http::StatusCode;
use hl_container::NetworkDriver;
use std::collections::BTreeMap;

use super::{ApiError, ApiResult};

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
            if ![
                "driver", "id", "label", "label!", "name", "scope", "type", "until",
            ]
            .contains(&name.as_str())
            {
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
        self.values("driver", |value| {
            value
                == match network.driver {
                    NetworkDriver::None => "none",
                    NetworkDriver::Bridge => "bridge",
                }
        }) && self.values("id", |value| network.id.as_str().starts_with(value))
            && self.values("name", |value| network.name.contains(value))
            && self.values("scope", |value| value == "local")
            && self.values("type", |value| {
                value
                    == if network.predefined() {
                        "builtin"
                    } else {
                        "custom"
                    }
            })
            && self.all_values("label!", |value| {
                !Self::matches_label(&network.labels, value)
            })
            && self.values("until", |value| {
                value
                    .parse::<crate::api::filter::PruneCutoff>()
                    .is_ok_and(|until| network.created_at_ms < until.milliseconds())
            })
            && self.values("label", |value| Self::matches_label(&network.labels, value))
    }

    fn values(&self, name: &str, predicate: impl Fn(&String) -> bool) -> bool {
        self.0
            .get(name)
            .is_none_or(|values| values.iter().any(predicate))
    }

    fn all_values(&self, name: &str, predicate: impl Fn(&String) -> bool) -> bool {
        self.0
            .get(name)
            .is_none_or(|values| values.iter().all(predicate))
    }

    fn matches_label(labels: &BTreeMap<String, String>, value: &str) -> bool {
        value.split_once('=').map_or_else(
            || labels.contains_key(value),
            |(name, expected)| labels.get(name).is_some_and(|actual| actual == expected),
        )
    }
}
