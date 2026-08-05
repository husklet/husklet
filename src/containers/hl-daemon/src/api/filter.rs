use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(untagged)]
enum DockerFilterValues {
    Current(BTreeMap<String, bool>),
    Legacy(Vec<String>),
}

impl DockerFilterValues {
    fn terms(self) -> Vec<String> {
        match self {
            // Docker's current encoding is a string set. The boolean values are
            // retained for compatibility but do not enable or disable keys.
            Self::Current(values) => values.into_keys().collect(),
            Self::Legacy(values) => values,
        }
    }
}

pub(crate) fn docker_filter_values(raw: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    const MAX_ENCODED_BYTES: usize = 64 * 1024;
    const MAX_FILTERS: usize = 64;
    const MAX_TERMS: usize = 1_024;
    const MAX_NAME_BYTES: usize = 128;
    const MAX_TERM_BYTES: usize = 4 * 1024;

    if raw.len() > MAX_ENCODED_BYTES {
        return Err("Docker filters exceed 64 KiB".into());
    }
    let encoded =
        serde_json::from_str::<BTreeMap<String, DockerFilterValues>>(raw).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_FILTERS {
        return Err("Docker filters exceed 64 names".into());
    }
    let mut total_terms = 0_usize;
    let mut filters = BTreeMap::new();
    for (name, values) in encoded {
        if name.len() > MAX_NAME_BYTES {
            return Err("Docker filter name exceeds 128 bytes".into());
        }
        let values = values.terms();
        total_terms = total_terms
            .checked_add(values.len())
            .filter(|total| *total <= MAX_TERMS)
            .ok_or_else(|| "Docker filters exceed 1024 values".to_owned())?;
        if values.iter().any(|value| value.len() > MAX_TERM_BYTES) {
            return Err("Docker filter value exceeds 4 KiB".into());
        }
        filters.insert(name, values);
    }
    Ok(filters)
}

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
        let filters: BTreeMap<String, Vec<String>> =
            serde_json::from_str(raw).map_err(|error| format!("invalid container prune filters: {error}"))?;
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

#[cfg(feature = "runtime")]
pub(crate) struct PreparedList {
    selection: List,
    temporal: BTreeMap<String, Vec<Option<(u64, hl_container::ContainerId)>>>,
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
        self.filters.entry(key.to_owned()).or_default().push(value.into());
        self
    }
}

impl From<bool> for List {
    fn from(all: bool) -> Self {
        if all { Self::default().all() } else { Self::default() }
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
        list.filters = docker_filter_values(filters).map_err(|error| format!("invalid container filters: {error}"))?;
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
                        | "expose"
                        | "is-task"
                        | "publish"
                        | "volume"
                        | "before"
                        | "since"
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        if unsupported.is_empty() {
            for name in ["expose", "publish"] {
                for value in list.filters.get(name).into_iter().flatten() {
                    Self::exposed_port(value)?;
                }
            }
            for value in list.filters.get("health").into_iter().flatten() {
                if !matches!(value.as_str(), "starting" | "healthy" | "unhealthy" | "none") {
                    return Err(format!("invalid health filter {value:?}"));
                }
            }
            let mut task = None;
            for value in list.filters.get("is-task").into_iter().flatten() {
                let value = Self::task_filter(value)?;
                if task.is_some_and(|current| current != value) {
                    return Err("conflicting is-task boolean values".into());
                }
                task = Some(value);
            }
            Ok(list)
        } else {
            Err(format!("unsupported container filters: {}", unsupported.join(", ")))
        }
    }

    pub(crate) fn prepare(self, containers: &[hl_container::Container]) -> PreparedList {
        let mut temporal = BTreeMap::new();
        for key in ["before", "since"] {
            let Some(values) = self.filters.get(key) else {
                continue;
            };
            let ordering = values
                .iter()
                .map(|value| Self::reference(containers, value).map(Self::ordering_key))
                .collect();
            temporal.insert(key.into(), ordering);
        }
        PreparedList {
            selection: self,
            temporal,
        }
    }

    fn reference<'a>(containers: &'a [hl_container::Container], value: &str) -> Option<&'a hl_container::Container> {
        containers.iter().find(|container| {
            container.id.as_str().starts_with(value)
                || container
                    .spec
                    .name
                    .as_deref()
                    .is_some_and(|name| name == value.trim_start_matches('/'))
        })
    }

    fn ordering_key(container: &hl_container::Container) -> (u64, hl_container::ContainerId) {
        (container.created_at_ms, container.id.clone())
    }

    fn matches_non_temporal(container: &hl_container::Container, key: &str, value: &str) -> bool {
        match key {
            "name" => container.spec.name.as_deref().is_some_and(|name| name.contains(value)),
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
            "ancestor" => container
                .spec
                .image
                .as_ref()
                .is_some_and(|image| value.parse::<hl_images::Reference>().is_ok_and(|value| &value == image)),
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
            "expose" => Self::exposed_port(value).is_ok_and(|(start, end, protocol)| {
                protocol == "tcp"
                    && container
                        .spec
                        .ports
                        .iter()
                        .any(|port| (start..=end).contains(&port.guest))
            }),
            "is-task" => Self::task_filter(value).is_ok_and(|is_task| !is_task),
            "publish" => Self::exposed_port(value).is_ok_and(|(start, end, protocol)| {
                protocol == "tcp"
                    && container
                        .spec
                        .publish
                        .iter()
                        .any(|publish| (start..=end).contains(&publish.port.guest))
            }),
            "volume" => container.spec.mounts.iter().any(|mount| {
                (!mount.target.as_os_str().is_empty() && mount.target == std::path::Path::new(value))
                    || match &mount.source {
                        hl_container::MountSource::Bind(source) => source == std::path::Path::new(value),
                        hl_container::MountSource::Volume(name) | hl_container::MountSource::Anonymous(name) => {
                            name == value
                        }
                        hl_container::MountSource::Tmpfs(_) => value.is_empty(),
                    }
            }),
            _ => false,
        }
    }

    fn exposed_port(value: &str) -> Result<(u16, u16, &str), String> {
        let (ports, protocol) = value.split_once('/').map_or((value, "tcp"), |parts| parts);
        if !matches!(protocol, "tcp" | "udp" | "sctp") {
            return Err(format!("invalid expose protocol {protocol:?}"));
        }
        let (start, end) = ports.split_once('-').map_or((ports, ports), |parts| parts);
        let start = start
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("invalid exposed port {value:?}"))?;
        let end = end
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0 && *port >= start)
            .ok_or_else(|| format!("invalid exposed port {value:?}"))?;
        Ok((start, end, protocol))
    }

    fn task_filter(value: &str) -> Result<bool, String> {
        match value {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(format!("invalid is-task boolean {value:?}")),
        }
    }

    fn matches_label(container: &hl_container::Container, value: &str) -> bool {
        value.split_once('=').map_or_else(
            || container.spec.labels.contains_key(value),
            |(name, value)| container.spec.labels.get(name).is_some_and(|current| current == value),
        )
    }
}

#[cfg(feature = "runtime")]
impl PreparedList {
    pub(crate) const fn includes_inactive(&self) -> bool {
        self.selection.includes_inactive()
    }

    pub(crate) fn matches(&self, container: &hl_container::Container) -> bool {
        self.selection.filters.iter().all(|(key, values)| match key.as_str() {
            "before" => {
                values.is_empty()
                    || self.temporal[key]
                        .iter()
                        .flatten()
                        .any(|reference| &List::ordering_key(container) < reference)
            }
            "since" => {
                values.is_empty()
                    || self.temporal[key]
                        .iter()
                        .flatten()
                        .any(|reference| &List::ordering_key(container) > reference)
            }
            "label!" => values.iter().all(|value| !List::matches_label(container, value)),
            _ => {
                values.is_empty()
                    || values
                        .iter()
                        .any(|value| List::matches_non_temporal(container, key, value))
            }
        })
    }
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::{List, PruneCutoff, docker_filter_values};
    use hl_container::{
        Access, BindPropagation, Container, ContainerSpec, ContainerState, Mount, MountSource, Process,
    };
    use std::collections::BTreeMap;
    use std::str::FromStr as _;

    fn container() -> Container {
        let mut container = Container::new(
            "67ea8f51-9e4d-4f4f-957d-f834263fe522".parse().unwrap(),
            ContainerSpec::from_directory("/rootfs", Process::new("/bin/true"))
                .name("build-worker")
                .label("role", "build")
                .image(hl_images::Reference::from_str("registry.test/team/tool:7").unwrap()),
            ContainerState::Exited {
                result: hl_container::ExitStatus::Code(0),
                finished_at_ms: 1,
            },
            0,
        );
        container.generation = 1;
        container
    }

    fn matches(selection: List, container: &Container, containers: &[Container]) -> bool {
        selection.prepare(containers).matches(container)
    }

    #[test]
    fn until_accepts_unix_seconds_and_rfc3339_timestamps() {
        assert_eq!("12.3456".parse::<PruneCutoff>().unwrap().milliseconds(), 12_345);
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
        assert!(matches(selected, &container, std::slice::from_ref(&container)));
        assert!(!matches(
            List::default().status("running"),
            &container,
            std::slice::from_ref(&container)
        ));
    }

    #[test]
    fn parser_rejects_unknown_filters_instead_of_ignoring_them() {
        assert!(
            List::parse(false, Some(r#"{"unsupported":["value"]}"#))
                .unwrap_err()
                .contains("unsupported")
        );
        for filters in [r#"{"health":["bogus"]}"#, r#"{"health!":["healthy"]}"#] {
            assert!(List::parse(false, Some(filters)).is_err(), "accepted {filters}");
        }
    }

    #[test]
    fn parser_normalizes_current_map_sets_and_legacy_arrays() {
        let current = List::parse(
            true,
            Some(r#"{"name":{"worker":true},"status":{"exited":false},"label":{"role=build":true}}"#),
        )
        .unwrap();
        let legacy = List::parse(
            true,
            Some(r#"{"name":["worker"],"status":["exited"],"label":["role=build"]}"#),
        )
        .unwrap();

        assert_eq!(current, legacy);
    }

    #[test]
    fn expose_filter_matches_declared_tcp_ports_and_validates_ranges() {
        let mut exposed = container();
        exposed.spec = exposed
            .spec
            .clone()
            .expose(hl_container::Port::tcp(80).unwrap())
            .expose(hl_container::Port::tcp(443).unwrap());
        for value in ["80", "80/tcp", "79-80", "443-444/tcp"] {
            let selected = List::parse(true, Some(&format!(r#"{{"expose":["{value}"]}}"#))).unwrap();
            assert!(matches(selected, &exposed, std::slice::from_ref(&exposed)));
        }
        for value in ["81", "80/udp", "80/sctp"] {
            let selected = List::parse(true, Some(&format!(r#"{{"expose":["{value}"]}}"#))).unwrap();
            assert!(!matches(selected, &exposed, std::slice::from_ref(&exposed)));
        }
        let alternatives = List::parse(
            true,
            Some(r#"{"expose":{"81":false,"443":true},"status":["exited"]}"#),
        )
        .unwrap();
        assert!(matches(alternatives, &exposed, std::slice::from_ref(&exposed)));
        for value in ["", "0", "65536", "90-80", "80-", "-80", "80/icmp", "80/tcp/extra"] {
            assert!(List::parse(true, Some(&format!(r#"{{"expose":["{value}"]}}"#))).is_err());
        }
        let empty = container();
        let selected = List::parse(true, Some(r#"{"expose":["1-65535"]}"#)).unwrap();
        assert!(!matches(selected, &empty, std::slice::from_ref(&empty)));
    }

    #[test]
    fn is_task_filter_truth_table_preserves_only_ordinary_containers() {
        let container = container();
        for value in ["false", "0"] {
            let selected = List::parse(true, Some(&format!(r#"{{"is-task":["{value}"]}}"#))).unwrap();
            assert!(matches(selected, &container, std::slice::from_ref(&container)));
        }
        for value in ["true", "1"] {
            let selected = List::parse(true, Some(&format!(r#"{{"is-task":["{value}"]}}"#))).unwrap();
            assert!(!matches(selected, &container, std::slice::from_ref(&container)));
        }
        let alternatives = List::parse(true, Some(r#"{"is-task":["false","0"],"status":["exited"]}"#)).unwrap();
        assert!(matches(alternatives, &container, std::slice::from_ref(&container)));
        let map_set = List::parse(true, Some(r#"{"is-task":{"false":false}}"#)).unwrap();
        assert!(matches(map_set, &container, std::slice::from_ref(&container)));
        for value in ["", "yes", "False", "2"] {
            assert!(List::parse(true, Some(&format!(r#"{{"is-task":["{value}"]}}"#))).is_err());
        }
        assert!(List::parse(true, Some(r#"{"is-task":["true","false"]}"#)).is_err());
    }

    #[test]
    fn publish_filter_matches_only_published_container_ports() {
        let mut published = container();
        published.spec = published
            .spec
            .clone()
            .expose(hl_container::Port::tcp(81).unwrap())
            .publish(hl_container::Publication::tcp(std::net::Ipv4Addr::LOCALHOST, 8_080, 80).unwrap());
        for value in ["80", "80/tcp", "79-80", "80-81/tcp"] {
            let selected = List::parse(true, Some(&format!(r#"{{"publish":["{value}"]}}"#))).unwrap();
            assert!(matches(selected, &published, std::slice::from_ref(&published)));
        }
        for value in ["81", "80/udp", "80/sctp"] {
            let selected = List::parse(true, Some(&format!(r#"{{"publish":["{value}"]}}"#))).unwrap();
            assert!(!matches(selected, &published, std::slice::from_ref(&published)));
        }
        let alternatives =
            List::parse(true, Some(r#"{"publish":{"81":false,"80":true},"status":["exited"]}"#)).unwrap();
        assert!(matches(alternatives, &published, std::slice::from_ref(&published)));
        for value in ["", "0", "65536", "90-80", "80-", "-80", "80/icmp", "80/tcp/extra"] {
            assert!(List::parse(true, Some(&format!(r#"{{"publish":["{value}"]}}"#))).is_err());
        }
    }

    #[test]
    fn volume_filter_matches_moby_mount_names_sources_and_destinations() {
        let mut mounted = container();
        mounted.spec.mounts = vec![
            Mount::read_write("/host/source", "/bind-target"),
            Mount::volume_read_write("named-data", "/volume-target"),
            Mount {
                source: MountSource::Anonymous("anonymous-data".into()),
                target: "/anonymous-target".into(),
                access: Access::ReadWrite,
                populate: false,
                subpath: None,
                propagation: BindPropagation::RecursivePrivate,
                recursive: true,
            },
            Mount {
                source: MountSource::Tmpfs("tmpfs-storage".into()),
                target: "/tmpfs-target".into(),
                access: Access::ReadWrite,
                populate: false,
                subpath: None,
                propagation: BindPropagation::RecursivePrivate,
                recursive: true,
            },
        ];
        for value in [
            "/host/source",
            "/bind-target",
            "named-data",
            "/volume-target",
            "anonymous-data",
            "/anonymous-target",
            "/tmpfs-target",
            "",
        ] {
            let selected = List::parse(true, Some(&format!(r#"{{"volume":["{value}"]}}"#))).unwrap();
            assert!(matches(selected, &mounted, std::slice::from_ref(&mounted)), "{value:?}");
        }
        for value in ["tmpfs-storage", "/managed/backing/source", "missing"] {
            let selected = List::parse(true, Some(&format!(r#"{{"volume":["{value}"]}}"#))).unwrap();
            assert!(
                !matches(selected, &mounted, std::slice::from_ref(&mounted)),
                "{value:?}"
            );
        }
        let alternatives = List::parse(
            true,
            Some(r#"{"volume":{"missing":true,"named-data":false},"status":["exited"]}"#),
        )
        .unwrap();
        assert!(matches(alternatives, &mounted, std::slice::from_ref(&mounted)));
        let conjunction = List::parse(true, Some(r#"{"volume":["named-data"],"name":["missing"]}"#)).unwrap();
        assert!(!matches(conjunction, &mounted, std::slice::from_ref(&mounted)));
    }

    #[test]
    fn parser_bounds_filter_wire_input() {
        let oversized = format!(r#"{{"name":{{"{}":true}}}}"#, "x".repeat(64 * 1024));
        assert!(List::parse(true, Some(&oversized)).unwrap_err().contains("64 KiB"));
    }

    #[test]
    fn docker_filter_parser_enforces_structural_bounds_and_duplicate_contract() {
        let filters: BTreeMap<_, _> = (0..64)
            .map(|index| (format!("f{index}"), Vec::<String>::new()))
            .collect();
        assert_eq!(
            docker_filter_values(&serde_json::to_string(&filters).unwrap())
                .unwrap()
                .len(),
            64
        );
        let filters: BTreeMap<_, _> = (0..65)
            .map(|index| (format!("f{index}"), Vec::<String>::new()))
            .collect();
        assert!(
            docker_filter_values(&serde_json::to_string(&filters).unwrap())
                .unwrap_err()
                .contains("64 names")
        );

        let values = vec!["x"; 1_024];
        assert_eq!(
            docker_filter_values(&serde_json::json!({"name": values}).to_string()).unwrap()["name"].len(),
            1_024
        );
        let values = vec!["x"; 1_025];
        assert!(
            docker_filter_values(&serde_json::json!({"name": values}).to_string())
                .unwrap_err()
                .contains("1024")
        );
        assert!(
            docker_filter_values(&serde_json::json!({"x".repeat(129): []}).to_string())
                .unwrap_err()
                .contains("128 bytes")
        );
        assert!(
            docker_filter_values(&serde_json::json!({"name": ["x".repeat(4097)]}).to_string())
                .unwrap_err()
                .contains("4 KiB")
        );

        let duplicate_names = docker_filter_values(r#"{"name":["first"],"name":["last"]}"#).unwrap();
        assert_eq!(duplicate_names["name"], ["last"]);
        let duplicate_values = docker_filter_values(r#"{"name":["same","same"]}"#).unwrap();
        assert_eq!(duplicate_values["name"], ["same", "same"]);
        let map_set = docker_filter_values(r#"{"name":{"enabled":true,"disabled":false}}"#).unwrap();
        assert_eq!(map_set["name"], ["disabled", "enabled"]);
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

        assert!(matches(
            List::parse(true, Some(r#"{"exited":["137"],"health":["starting"]}"#)).unwrap(),
            &older,
            &containers
        ));
        assert!(matches(
            List::parse(true, Some(r#"{"before":["newer"]}"#)).unwrap(),
            &older,
            &containers
        ));
        assert!(matches(
            List::parse(true, Some(r#"{"since":["67ea8f51"]}"#)).unwrap(),
            &newer,
            &containers
        ));
        assert!(!matches(
            List::parse(true, Some(r#"{"health":["none"]}"#)).unwrap(),
            &older,
            &containers
        ));
    }

    #[test]
    fn temporal_references_resolve_exact_name_id_and_unique_prefix_once() {
        let mut first = container();
        first.created_at_ms = 10;
        let mut second = container();
        second.id = "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
            .parse()
            .unwrap();
        second.spec.name = Some("second".into());
        second.created_at_ms = 20;
        let mut similar = second.clone();
        similar.id = "89abcdef1123456789abcdef0123456789abcdef0123456789abcdef01234567"
            .parse()
            .unwrap();
        similar.spec.name = Some("similar".into());
        similar.created_at_ms = 30;
        let containers = [first.clone(), second.clone(), similar];

        assert!(
            List::parse(true, Some(r#"{"before":["/second"]}"#))
                .unwrap()
                .prepare(&containers)
                .matches(&first)
        );
        assert!(
            List::parse(
                true,
                Some(r#"{"since":["89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"]}"#)
            )
            .unwrap()
            .prepare(&containers)
            .matches(&containers[2])
        );
        assert!(
            List::parse(true, Some(r#"{"since":["67ea8f51"]}"#))
                .unwrap()
                .prepare(&containers)
                .matches(&second)
        );
        let mut between = first.clone();
        between.created_at_ms = 25;
        assert!(
            !List::parse(true, Some(r#"{"before":["89abcdef"]}"#))
                .unwrap()
                .prepare(&containers)
                .matches(&between)
        );
        assert!(
            !List::parse(true, Some(r#"{"before":["missing"]}"#))
                .unwrap()
                .prepare(&containers)
                .matches(&first)
        );
    }

    #[test]
    fn temporal_boundaries_use_id_order_when_timestamps_match() {
        let mut before = container();
        before.id = "1111111111111111111111111111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        before.spec.name = Some("before".into());
        before.created_at_ms = 20;
        let mut boundary = before.clone();
        boundary.id = "2222222222222222222222222222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        boundary.spec.name = Some("boundary".into());
        let mut since = before.clone();
        since.id = "3333333333333333333333333333333333333333333333333333333333333333"
            .parse()
            .unwrap();
        since.spec.name = Some("since".into());
        let containers = [before.clone(), boundary, since.clone()];

        let before_selection = List::parse(true, Some(r#"{"before":["boundary"]}"#))
            .unwrap()
            .prepare(&containers);
        assert!(before_selection.matches(&before));
        assert!(!before_selection.matches(&since));

        let since_selection = List::parse(true, Some(r#"{"since":["boundary"]}"#))
            .unwrap()
            .prepare(&containers);
        assert!(since_selection.matches(&since));
        assert!(!since_selection.matches(&before));
    }
}
