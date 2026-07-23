use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Docker container-prune filters converted to domain selection.
#[cfg(feature = "runtime")]
pub(crate) struct Prune(hl_container::Prune);

/// Docker's `until` filter normalized to milliseconds since the Unix epoch.
#[cfg(feature = "runtime")]
pub(crate) struct PruneCutoff(u64);

#[cfg(feature = "runtime")]
impl PruneCutoff {
    pub(crate) const fn milliseconds(&self) -> u64 {
        self.0
    }
}

#[cfg(feature = "runtime")]
impl std::str::FromStr for PruneCutoff {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (seconds, fraction) = value.split_once('.').map_or((value, ""), |parts| parts);
        if !seconds.is_empty()
            && seconds.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            let seconds = seconds
                .parse::<u64>()
                .map_err(|_| "container prune until exceeds u64 seconds".to_owned())?;
            let mut milliseconds = fraction
                .bytes()
                .take(3)
                .fold(0_u64, |value, byte| value * 10 + u64::from(byte - b'0'));
            for _ in fraction.len().min(3)..3 {
                milliseconds *= 10;
            }
            return seconds
                .checked_mul(1_000)
                .and_then(|value| value.checked_add(milliseconds))
                .map(Self)
                .ok_or_else(|| "container prune until exceeds u64 milliseconds".into());
        }
        chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| format!("invalid container prune until value {value:?}"))?
            .timestamp_millis()
            .try_into()
            .map(Self)
            .map_err(|_| "container prune until must not predate the Unix epoch".into())
    }
}

#[cfg(feature = "runtime")]
impl Prune {
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw.filter(|value| !value.is_empty()) else {
            return Ok(Self(hl_container::Prune::default()));
        };
        let filters: BTreeMap<String, Vec<String>> = serde_json::from_str(raw)
            .map_err(|error| format!("invalid container prune filters: {error}"))?;
        let unsupported = filters
            .keys()
            .filter(|key| !matches!(key.as_str(), "until" | "label" | "label!"))
            .cloned()
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(format!(
                "unsupported container prune filters: {}",
                unsupported.join(", ")
            ));
        }
        let mut selection = hl_container::Prune::default();
        if let Some(values) = filters.get("until") {
            let [value] = values.as_slice() else {
                return Err("container prune until requires exactly one value".into());
            };
            selection = selection.before(value.parse::<PruneCutoff>()?.milliseconds());
        }
        for value in filters.get("label").into_iter().flatten() {
            selection = selection.label(value);
        }
        for value in filters.get("label!").into_iter().flatten() {
            selection = selection.without_label(value);
        }
        Ok(Self(selection))
    }

    pub(crate) const fn selection(&self) -> &hl_container::Prune {
        &self.0
    }
}

/// Selection applied by Docker's container-list endpoint.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct List {
    #[serde(default)]
    all: bool,
    #[serde(default)]
    filters: BTreeMap<String, Vec<String>>,
}

impl List {
    /// Includes containers that are not currently active.
    #[must_use]
    pub const fn all(mut self) -> Self {
        self.all = true;
        self
    }

    /// Selects containers whose name contains `value`.
    #[must_use]
    pub fn name(self, value: impl Into<String>) -> Self {
        self.select("name", value)
    }

    /// Selects containers whose identifier starts with `value`.
    #[must_use]
    pub fn id(self, value: impl Into<String>) -> Self {
        self.select("id", value)
    }

    /// Selects containers with the given Docker state name.
    #[must_use]
    pub fn status(self, value: impl Into<String>) -> Self {
        self.select("status", value)
    }

    /// Selects containers created from an image name or identifier.
    #[must_use]
    pub fn ancestor(self, value: impl Into<String>) -> Self {
        self.select("ancestor", value)
    }

    /// Selects containers carrying an exact label pair.
    #[must_use]
    pub fn label(self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.select("label", format!("{}={}", name.as_ref(), value.as_ref()))
    }

    /// Whether inactive containers are included.
    #[must_use]
    pub const fn includes_inactive(&self) -> bool {
        self.all
    }

    /// Docker filter names and their accepted alternatives.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, Vec<String>> {
        &self.filters
    }

    fn select(mut self, key: &str, value: impl Into<String>) -> Self {
        self.filters
            .entry(key.to_owned())
            .or_default()
            .push(value.into());
        self
    }
}

impl From<bool> for List {
    fn from(all: bool) -> Self {
        if all {
            Self::default().all()
        } else {
            Self::default()
        }
    }
}

#[cfg(feature = "runtime")]
impl List {
    pub(crate) fn parse(all: bool, filters: Option<&str>) -> Result<Self, String> {
        let mut list = Self {
            all,
            filters: BTreeMap::new(),
        };
        let Some(filters) = filters.filter(|value| !value.is_empty()) else {
            return Ok(list);
        };
        list.filters = serde_json::from_str(filters)
            .map_err(|error| format!("invalid container filters: {error}"))?;
        let unsupported = list
            .filters
            .keys()
            .filter(|key| {
                !matches!(
                    key.as_str(),
                    "name"
                        | "id"
                        | "status"
                        | "ancestor"
                        | "label"
                        | "label!"
                        | "exited"
                        | "health"
                        | "before"
                        | "since"
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        if unsupported.is_empty() {
            Ok(list)
        } else {
            Err(format!(
                "unsupported container filters: {}",
                unsupported.join(", ")
            ))
        }
    }

    pub(crate) fn matches_in(
        &self,
        container: &hl_container::Container,
        containers: &[hl_container::Container],
    ) -> bool {
        self.filters.iter().all(|(key, values)| match key.as_str() {
            "label!" => values
                .iter()
                .all(|value| !Self::matches_label(container, value)),
            _ => {
                values.is_empty()
                    || values
                        .iter()
                        .any(|value| Self::matches_one(container, containers, key, value))
            }
        })
    }

    fn matches_one(
        container: &hl_container::Container,
        containers: &[hl_container::Container],
        key: &str,
        value: &str,
    ) -> bool {
        match key {
            "name" => container
                .spec
                .name
                .as_deref()
                .is_some_and(|name| name.contains(value)),
            "id" => container.id.as_str().starts_with(value),
            "status" => {
                use hl_container::ContainerState;

                let status = match &container.state {
                    ContainerState::Created => "created",
                    ContainerState::Running { .. } => "running",
                    ContainerState::Paused { .. } => "paused",
                    ContainerState::Restarting { .. } => "restarting",
                    ContainerState::Exited { .. } => "exited",
                };
                status == value
            }
            "ancestor" => container.spec.image.as_ref().is_some_and(|image| {
                value
                    .parse::<hl_images::Reference>()
                    .is_ok_and(|value| &value == image)
            }),
            "label" => Self::matches_label(container, value),
            "exited" => matches!(
                &container.state,
                hl_container::ContainerState::Exited { result, .. }
                    if value.parse::<i32>().is_ok_and(|expected| {
                        let code = match *result {
                            hl_container::ExitStatus::Code(code) => code,
                            hl_container::ExitStatus::Signal(signal) => 128 + signal,
                            hl_container::ExitStatus::Fault { status, .. } => status,
                        };
                        code == expected
                    })
            ),
            "health" => {
                let health = match container.health.as_ref().map(|health| health.status) {
                    Some(hl_container::HealthStatus::Starting) => "starting",
                    Some(hl_container::HealthStatus::Healthy) => "healthy",
                    Some(hl_container::HealthStatus::Unhealthy) => "unhealthy",
                    None => "none",
                };
                health == value
            }
            "before" => Self::reference(containers, value)
                .is_some_and(|reference| container.created_at_ms < reference.created_at_ms),
            "since" => Self::reference(containers, value)
                .is_some_and(|reference| container.created_at_ms > reference.created_at_ms),
            _ => false,
        }
    }

    fn reference<'a>(
        containers: &'a [hl_container::Container],
        value: &str,
    ) -> Option<&'a hl_container::Container> {
        containers.iter().find(|container| {
            container.id.as_str().starts_with(value)
                || container
                    .spec
                    .name
                    .as_deref()
                    .is_some_and(|name| name == value.trim_start_matches('/'))
        })
    }

    fn matches_label(container: &hl_container::Container, value: &str) -> bool {
        value.split_once('=').map_or_else(
            || container.spec.labels.contains_key(value),
            |(name, value)| {
                container
                    .spec
                    .labels
                    .get(name)
                    .is_some_and(|current| current == value)
            },
        )
    }
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::{List, PruneCutoff};
    use hl_container::{Container, ContainerSpec, ContainerState, Process};
    use std::str::FromStr as _;

    fn container() -> Container {
        Container {
            id: "67ea8f51-9e4d-4f4f-957d-f834263fe522".parse().unwrap(),
            spec: ContainerSpec::from_directory("/rootfs", Process::new("/bin/true"))
                .name("build-worker")
                .label("role", "build")
                .image(hl_images::Reference::from_str("registry.test/team/tool:7").unwrap()),
            state: ContainerState::Exited {
                result: hl_container::ExitStatus::Code(0),
                finished_at_ms: 1,
            },
            created_at_ms: 0,
            generation: 1,
            restart: hl_container::Restart::default(),
            health: None,
            checkpoint: None,
        }
    }

    #[test]
    fn until_accepts_unix_seconds_and_rfc3339_timestamps() {
        assert_eq!(
            "12.3456".parse::<PruneCutoff>().unwrap().milliseconds(),
            12_345
        );
        assert_eq!(
            "1970-01-01T00:00:12.345Z"
                .parse::<PruneCutoff>()
                .unwrap()
                .milliseconds(),
            12_345
        );
    }

    #[test]
    fn until_rejects_invalid_negative_and_overflowing_values() {
        for value in ["invalid", "1969-12-31T23:59:59Z", "18446744073709552"] {
            assert!(value.parse::<PruneCutoff>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn alternatives_or_within_a_filter_and_across_filters() {
        let selected = List::default()
            .name("other")
            .name("worker")
            .status("exited")
            .id("67ea8f51")
            .ancestor("registry.test/team/tool:7");
        let selected = selected.label("role", "build");
        let container = container();
        assert!(selected.matches_in(&container, std::slice::from_ref(&container)));
        assert!(!List::default()
            .status("running")
            .matches_in(&container, std::slice::from_ref(&container)));
    }

    #[test]
    fn parser_rejects_unknown_filters_instead_of_ignoring_them() {
        assert!(List::parse(false, Some(r#"{"unsupported":["value"]}"#))
            .unwrap_err()
            .contains("unsupported"));
    }

    #[test]
    fn lifecycle_health_and_relative_filters_use_durable_state() {
        let mut older = container();
        older.created_at_ms = 10;
        older.state = ContainerState::Exited {
            result: hl_container::ExitStatus::Code(137),
            finished_at_ms: 20,
        };
        older.health = Some(hl_container::Health::starting());

        let mut newer = container();
        newer.id = "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
            .parse()
            .unwrap();
        newer.spec.name = Some("newer".into());
        newer.created_at_ms = 30;
        let containers = [older.clone(), newer.clone()];

        assert!(
            List::parse(true, Some(r#"{"exited":["137"],"health":["starting"]}"#))
                .unwrap()
                .matches_in(&older, &containers)
        );
        assert!(List::parse(true, Some(r#"{"before":["newer"]}"#))
            .unwrap()
            .matches_in(&older, &containers));
        assert!(List::parse(true, Some(r#"{"since":["67ea8f51"]}"#))
            .unwrap()
            .matches_in(&newer, &containers));
        assert!(!List::parse(true, Some(r#"{"health":["none"]}"#))
            .unwrap()
            .matches_in(&older, &containers));
    }
}
