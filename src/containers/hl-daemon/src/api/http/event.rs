use axum::body::Body;
use axum::extract::{Query, State};
use axum::response::Response;
use bytes::Bytes;
use futures_util::stream;
use http::StatusCode;
use hyper::body::Frame;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::convert::Infallible;

use crate::api::filter::docker_filter_values;
use crate::api::{EventFilter, EventQuery};

use super::{ApiError, ApiResult, DockerState};

#[derive(Default, Deserialize)]
pub(super) struct QueryParameters {
    filters: Option<String>,
    since: Option<String>,
    until: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

impl QueryParameters {
    fn event_query(self) -> ApiResult<EventQuery> {
        if let Some(key) = self.unsupported.keys().next() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported events query option {key:?}"),
            ));
        }
        let filters = self
            .filters
            .map(|value| docker_filter_values(&value))
            .transpose()
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.clone()))?
            .unwrap_or_default();
        for key in filters.keys() {
            if !matches!(
                key.as_str(),
                "type" | "event" | "action" | "container" | "image" | "network" | "volume" | "label"
            ) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unsupported event filter {key:?}"),
                ));
            }
        }
        let now = chrono::Utc::now().timestamp();
        let query = EventQuery {
            filters: EventFilter(filters),
            since: self
                .since
                .as_deref()
                .map(str::parse::<Time>)
                .transpose()?
                .map(|time| time.at(now)),
            until: self
                .until
                .as_deref()
                .map(str::parse::<Time>)
                .transpose()?
                .map(|time| time.at(now)),
        };
        if matches!((query.since, query.until), (Some(since), Some(until)) if since > until) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "events since must not be later than until",
            ));
        }
        Ok(query)
    }
}

enum Time {
    Absolute(i64),
    Ago(i64),
}

impl Time {
    const fn at(self, now: i64) -> i64 {
        match self {
            Self::Absolute(seconds) => seconds,
            Self::Ago(seconds) => now.saturating_sub(seconds),
        }
    }
}

impl std::str::FromStr for Time {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if let Ok(seconds) = value.parse::<i64>() {
            return Ok(Self::Absolute(seconds));
        }
        if let Some((seconds, fraction)) = value.split_once('.')
            && !fraction.is_empty()
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(seconds) = seconds.parse::<i64>()
        {
            return Ok(Self::Absolute(seconds));
        }
        if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
            return Ok(Self::Absolute(timestamp.timestamp()));
        }

        let mut total = 0_i64;
        let mut digits = String::new();
        let mut units = 0;
        for character in value.chars() {
            if character.is_ascii_digit() {
                digits.push(character);
                continue;
            }
            let number = digits.parse::<i64>().map_err(|_| Self::invalid(value))?;
            digits.clear();
            let multiplier = match character {
                'h' => 3_600,
                'm' => 60,
                's' => 1,
                _ => return Err(Self::invalid(value)),
            };
            total = total
                .checked_add(number.checked_mul(multiplier).ok_or_else(|| Self::invalid(value))?)
                .ok_or_else(|| Self::invalid(value))?;
            units += 1;
        }
        if units == 0 || !digits.is_empty() {
            return Err(Self::invalid(value));
        }
        Ok(Self::Ago(total))
    }
}

impl Time {
    fn invalid(value: &str) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, format!("invalid Docker time {value:?}"))
    }
}

#[hl_design::adapter]
pub(super) async fn get(State(state): State<DockerState>, Query(query): Query<QueryParameters>) -> ApiResult<Response> {
    let subscription = state.events.subscribe(query.event_query()?);
    let body = stream::unfold(subscription, |mut subscription| async move {
        let event = subscription.next().await?;
        let line = event.line().expect("event wire model is serializable");
        Some((Ok::<_, Infallible>(Frame::data(Bytes::from(line))), subscription))
    });
    Response::builder()
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::new(http_body_util::StreamBody::new(body)))
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_docker_ts_unix_seconds() {
        assert_eq!("1700000000".parse::<Time>().unwrap().at(0), 1_700_000_000);
        assert_eq!("1700000000.5".parse::<Time>().unwrap().at(0), 1_700_000_000);
    }

    #[test]
    fn parse_docker_ts_rfc3339() {
        assert_eq!("2023-11-14T22:13:20Z".parse::<Time>().unwrap().at(0), 1_700_000_000);
        assert_eq!(
            "2023-11-15T00:13:20+02:00".parse::<Time>().unwrap().at(0),
            1_700_000_000
        );
    }

    #[test]
    fn parse_docker_ts_go_duration_relative_to_now() {
        assert_eq!("10m".parse::<Time>().unwrap().at(1_000), 400);
        assert_eq!("1h30m".parse::<Time>().unwrap().at(10_000), 4_600);
        assert_eq!("90s".parse::<Time>().unwrap().at(1_000), 910);
    }

    #[test]
    fn parse_docker_ts_rejects_garbage() {
        assert!("not-a-time".parse::<Time>().is_err());
        assert!("".parse::<Time>().is_err());
    }

    #[test]
    fn query_accepts_docker_times_and_rejects_unknown_options_and_filters() {
        let query = QueryParameters {
            filters: Some(r#"{"image":["alpine"],"network":["frontend"],"volume":["data"]}"#.into()),
            since: Some("1970-01-01T00:00:02Z".into()),
            until: Some("3".into()),
            unsupported: BTreeMap::new(),
        }
        .event_query()
        .unwrap();
        assert_eq!(query.since, Some(2));
        assert_eq!(query.until, Some(3));
        assert_eq!("4.123456789".parse::<Time>().unwrap().at(0), 4);

        let unknown = QueryParameters {
            unsupported: [("stream".into(), "1".into())].into_iter().collect(),
            ..QueryParameters::default()
        };
        assert!(unknown.event_query().is_err());

        let unsupported_filter = QueryParameters {
            filters: Some(r#"{"scope":["local"]}"#.into()),
            ..QueryParameters::default()
        };
        assert!(unsupported_filter.event_query().is_err());

        let reversed = QueryParameters {
            since: Some("4".into()),
            until: Some("3".into()),
            ..QueryParameters::default()
        };
        assert!(reversed.event_query().is_err());
    }

    #[test]
    fn query_accepts_current_map_and_legacy_array_filter_encodings() {
        let current = QueryParameters {
            filters: Some(r#"{"type":{"volume":true},"event":{"create":false}}"#.into()),
            ..QueryParameters::default()
        }
        .event_query()
        .unwrap();
        assert_eq!(current.filters.0["type"], ["volume"]);
        assert_eq!(current.filters.0["event"], ["create"]);

        let legacy = QueryParameters {
            filters: Some(r#"{"type":["volume"],"event":["create"]}"#.into()),
            ..QueryParameters::default()
        }
        .event_query()
        .unwrap();
        assert_eq!(legacy.filters, current.filters);
    }

    #[test]
    fn query_rejects_malformed_filters_json() {
        let malformed = QueryParameters {
            filters: Some(r#"{"event":["start"]"#.into()),
            ..QueryParameters::default()
        };
        assert!(malformed.event_query().is_err());

        let wrong_value_shape = QueryParameters {
            filters: Some(r#"{"event":"start"}"#.into()),
            ..QueryParameters::default()
        };
        assert!(wrong_value_shape.event_query().is_err());
    }

    #[test]
    fn query_parses_all_supported_filter_keys() {
        let query = QueryParameters {
            filters: Some(
                r#"{"type":["container"],"event":["start"],"action":["die"],"container":["one"],"image":["alpine"],"network":["front"],"volume":["data"],"label":["tier=api"]}"#.into(),
            ),
            ..QueryParameters::default()
        }
        .event_query()
        .unwrap();

        for key in [
            "type",
            "event",
            "action",
            "container",
            "image",
            "network",
            "volume",
            "label",
        ] {
            assert!(query.filters.0.contains_key(key), "missing {key}");
        }
    }
}
