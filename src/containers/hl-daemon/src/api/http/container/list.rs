use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct ListQuery {
    #[serde(default, deserialize_with = "crate::api::http::query::flag")]
    all: bool,
    filters: Option<String>,
    limit: Option<String>,
    size: Option<String>,
}

impl ListQuery {
    fn limit(&self) -> ApiResult<Option<usize>> {
        let Some(value) = self.limit.as_deref().filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let value = value
            .parse::<i64>()
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, format!("invalid container limit {value:?}")))?;
        if value <= 0 {
            return Ok(None);
        }
        usize::try_from(value)
            .map(Some)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "container limit exceeds host range"))
    }

    fn include_size(&self) -> ApiResult<bool> {
        self.size.as_deref().unwrap_or_default().parse::<Flag>().map(bool::from)
    }

    fn select(&self, mut values: Vec<hl_container::Container>) -> ApiResult<Vec<hl_container::Container>> {
        let limit = self.limit()?;
        let selection = crate::api::List::parse(self.all || limit.is_some(), self.filters.as_deref())
            .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))?;
        let selection = selection.prepare(&values);
        values.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(values
            .iter()
            .filter(|value| selection.includes_inactive() || value.state.is_active())
            .filter(|value| selection.matches(value))
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
            .collect())
    }
}

#[derive(Clone, Debug)]
pub(super) struct NetworkAttachment {
    pub(super) name: String,
    endpoint: hl_container::EndpointSpec,
}

#[derive(Clone, Debug)]
pub(super) struct NetworkPlan {
    pub(super) attachments: Vec<NetworkAttachment>,
    prepare_bridge: bool,
    prepare_none: bool,
    isolated: bool,
    mode: hl_container::NetworkMode,
}

impl NetworkPlan {
    pub(super) fn from_request(host: Option<&HostConfig>, config: Option<&NetworkingConfig>) -> ApiResult<Self> {
        let mode = host.map_or("", |host| host.network_mode.as_str());
        let mut endpoints = config
            .map(|config| config.endpoints_config.0.clone())
            .unwrap_or_default();
        if matches!(mode, "" | "default" | "bridge")
            && let Some(endpoint) = endpoints.remove("default")
                && endpoints.insert(DEFAULT_NETWORK.into(), endpoint).is_some() {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "default and bridge endpoints select the same network",
                    ));
                }
        if mode == "host" {
            if host.is_some_and(|host| !host.links.is_empty()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "host NetworkMode cannot be combined with Links",
                ));
            }
            if !endpoints.is_empty() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "host NetworkMode cannot be combined with NetworkingConfig endpoints",
                ));
            }
            return Ok(Self {
                attachments: Vec::new(),
                prepare_bridge: false,
                prepare_none: false,
                isolated: false,
                mode: hl_container::NetworkMode::Host,
            });
        }
        if mode == "none" && !endpoints.is_empty() && (endpoints.len() > 1 || !endpoints.contains_key("none")) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "none NetworkMode only accepts the none endpoint",
            ));
        }
        if !matches!(mode, "" | "default" | "bridge" | "none") && !endpoints.is_empty() && !endpoints.contains_key(mode)
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "NetworkingConfig must contain the HostConfig.NetworkMode endpoint",
            ));
        }
        if mode == "bridge" && !endpoints.is_empty() && !endpoints.contains_key(DEFAULT_NETWORK) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bridge NetworkMode requires the bridge endpoint",
            ));
        }
        let endpoints = if endpoints.is_empty() {
            let name = match mode {
                "" | "default" | "bridge" => DEFAULT_NETWORK,
                "none" => "none",
                name => name,
            };
            BTreeMap::from([(name.to_owned(), EndpointConfig::default())])
        } else {
            endpoints
        };
        let prepare_bridge = endpoints.contains_key(DEFAULT_NETWORK);
        let prepare_none = endpoints.contains_key("none");
        let attachments = endpoints
            .into_iter()
            .map(|(name, endpoint)| endpoint.spec().map(|endpoint| NetworkAttachment { name, endpoint }))
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(Self {
            attachments,
            prepare_bridge,
            prepare_none,
            isolated: false,
            mode: hl_container::NetworkMode::Automatic,
        })
    }

    pub(super) const fn isolated(&self) -> bool {
        self.isolated
    }

    pub(super) const fn mode(&self) -> hl_container::NetworkMode {
        self.mode
    }

    pub(super) async fn prepare(mut self, containers: &hl_container::Containers) -> ApiResult<Self> {
        if self.mode == hl_container::NetworkMode::Host {
            return Ok(self);
        }
        if self.prepare_bridge {
            Self::ensure_bridge(containers).await?;
        }
        if self.prepare_none {
            Self::ensure_none(containers).await?;
        }
        let requests = self
            .attachments
            .iter()
            .map(|attachment| (attachment.name.clone(), attachment.endpoint.clone()))
            .collect::<Vec<_>>();
        let drivers = containers
            .networks()
            .validate_connections(&requests)
            .await
            .map_err(ApiError::container)?;
        let driver = drivers.first().copied();
        for candidate in drivers {
            if driver.is_some_and(|value| value != candidate) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "none and bridge networks cannot be attached to the same container",
                ));
            }
        }
        self.isolated = driver == Some(NetworkDriver::None);
        Ok(self)
    }

    pub(super) async fn attach_created(self, containers: &hl_container::Containers, container: &str) -> ApiResult<()> {
        if self.attachments.is_empty() {
            return Ok(());
        }
        let requests = self
            .attachments
            .into_iter()
            .map(|attachment| (attachment.name, attachment.endpoint));
        if let Err(error) = containers.networks().connect_many(container, requests).await {
            let error = ApiError::container(error);
            let cleanup = containers.remove_volumes(container, false).await;
            return match cleanup {
                Ok(_) => Err(error),
                Err(cleanup) => Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("network attachment failed ({error:?}); container rollback also failed ({cleanup})"),
                )),
            };
        }
        Ok(())
    }

    async fn ensure_none(containers: &hl_container::Containers) -> ApiResult<()> {
        match containers.networks().inspect("none").await {
            Ok(network) if network.driver == NetworkDriver::None => return Ok(()),
            Ok(_) => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "none network name belongs to another network driver",
                ));
            }
            Err(ContainerError::NetworkNotFound(_)) => {}
            Err(error) => return Err(ApiError::container(error)),
        }
        match containers.networks().create(NetworkSpec::none("none")).await {
            Ok(_) => Ok(()),
            Err(ContainerError::NetworkConflict(_)) => containers
                .networks()
                .inspect("none")
                .await
                .and_then(|network| {
                    if network.driver == NetworkDriver::None {
                        Ok(network)
                    } else {
                        Err(ContainerError::NetworkConflict("none".into()))
                    }
                })
                .map(|_| ())
                .map_err(ApiError::container),
            Err(error) => Err(ApiError::container(error)),
        }
    }

    pub(super) async fn ensure_bridge(containers: &hl_container::Containers) -> ApiResult<()> {
        match containers.networks().inspect(DEFAULT_NETWORK).await {
            Ok(network) if network.driver == NetworkDriver::Bridge => return Ok(()),
            Ok(_) => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "default bridge name belongs to another network driver",
                ));
            }
            Err(ContainerError::NetworkNotFound(_)) => {}
            Err(error) => return Err(ApiError::container(error)),
        }
        for second in [31_u8, 30, 29, 28] {
            for third in 0_u8..=255 {
                let subnet =
                    Subnet::new(std::net::Ipv4Addr::new(172, second, third, 0), 24).map_err(ApiError::container)?;
                match containers
                    .networks()
                    .create(NetworkSpec::bridge(DEFAULT_NETWORK, subnet))
                    .await
                {
                    Ok(_) => return Ok(()),
                    Err(ContainerError::InvalidNetwork(message)) if message.contains("overlaps") => {}
                    Err(ContainerError::NetworkConflict(_)) => {
                        return containers
                            .networks()
                            .inspect(DEFAULT_NETWORK)
                            .await
                            .and_then(|network| {
                                if network.driver == NetworkDriver::Bridge {
                                    Ok(network)
                                } else {
                                    Err(ContainerError::NetworkConflict(DEFAULT_NETWORK.into()))
                                }
                            })
                            .map(|_| ())
                            .map_err(ApiError::container);
                    }
                    Err(error) => return Err(ApiError::container(error)),
                }
            }
        }
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "no non-overlapping default bridge subnet is available",
        ))
    }
}

#[hl_design::adapter]
pub(in super::super) async fn list(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<Container>>> {
    let values = state.containers.list().await.map_err(ApiError::container)?;
    let include_size = query.include_size()?;
    let selected = query.select(values)?;
    Ok(Json(
        super::size::summaries(&state.containers, selected, include_size).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_container::{ContainerId, ContainerState, Process};

    fn container(name: &str, created_at_ms: u64) -> hl_container::Container {
        hl_container::Container::new(
            format!("{created_at_ms:032x}").parse::<ContainerId>().unwrap(),
            ContainerSpec::from_directory("/rootfs", Process::new("/bin/true")).name(name),
            ContainerState::Created,
            created_at_ms,
        )
    }

    fn names(values: &[hl_container::Container]) -> Vec<String> {
        values
            .iter()
            .map(|value| value.spec.name.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn newest_first() {
        let query = ListQuery {
            all: true,
            filters: None,
            limit: None,
            size: None,
        };
        let values = query
            .select(vec![
                container("old", 10),
                container("new", 30),
                container("middle", 20),
            ])
            .unwrap();
        assert_eq!(names(&values), ["new", "middle", "old"]);
    }

    #[test]
    fn limit_forms() {
        for value in [None, Some(""), Some("0"), Some("-2")] {
            let query = ListQuery {
                all: true,
                filters: None,
                limit: value.map(str::to_owned),
                size: None,
            };
            assert_eq!(query.limit().unwrap(), None);
            assert_eq!(
                names(&query.select(vec![container("old", 1), container("new", 2)]).unwrap()),
                ["new", "old"]
            );
        }
        let query = ListQuery {
            all: false,
            filters: None,
            limit: Some("2".into()),
            size: None,
        };
        assert_eq!(query.limit().unwrap(), Some(2));
        for value in ["two", "9223372036854775808"] {
            let query = ListQuery {
                all: false,
                filters: None,
                limit: Some(value.into()),
                size: None,
            };
            assert_eq!(
                query.select(vec![container("value", 1)]).unwrap_err().status,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn filtered_limit() {
        let mut old = container("old-match", 10);
        old.spec.labels.insert("role".into(), "keep".into());
        let mut new = container("new-match", 20);
        new.spec.labels.insert("role".into(), "keep".into());
        let query = ListQuery {
            all: false,
            filters: Some(r#"{"label":["role=keep"]}"#.into()),
            limit: Some("1".into()),
            size: None,
        };
        let values = query.select(vec![old, container("newest-other", 30), new]).unwrap();
        assert_eq!(names(&values), ["new-match"]);
    }

    #[test]
    fn temporal_prefix_resolution_preserves_inventory_order() {
        let mut first_match = container("first-match", 20);
        first_match.id = "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
            .parse()
            .unwrap();
        let mut newer_match = container("newer-match", 30);
        newer_match.id = "89abcdef1123456789abcdef0123456789abcdef0123456789abcdef01234567"
            .parse()
            .unwrap();
        let candidate = container("candidate", 25);
        let query = ListQuery {
            all: true,
            filters: Some(r#"{"before":["89abcdef"]}"#.into()),
            limit: None,
            size: None,
        };

        let values = query.select(vec![first_match, newer_match, candidate]).unwrap();
        assert!(values.is_empty());
    }

    #[test]
    fn size_flag() {
        for (value, expected) in [(None, false), (Some("0"), false), (Some("true"), true)] {
            let query = ListQuery {
                size: value.map(str::to_owned),
                ..ListQuery::default()
            };
            assert_eq!(query.include_size().unwrap(), expected);
        }
        let query = ListQuery {
            size: Some("maybe".into()),
            ..ListQuery::default()
        };
        assert_eq!(query.include_size().unwrap_err().status, StatusCode::BAD_REQUEST);
    }
}

#[derive(Default, Deserialize)]
pub(in super::super) struct PruneQuery {
    pub(in super::super) filters: Option<String>,
}

#[hl_design::adapter]
pub(in super::super) async fn prune(
    State(state): State<DockerState>,
    Query(query): Query<PruneQuery>,
) -> ApiResult<Json<ContainerPrune>> {
    let selection = crate::api::filter::Prune::parse(query.filters.as_deref())
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;
    let removed = state
        .containers
        .prune(selection.selection())
        .await
        .map_err(ApiError::container)?;
    Ok(Json(ContainerPrune {
        containers_deleted: removed.into_iter().map(|container| container.id.to_string()).collect(),
        space_reclaimed: 0,
    }))
}
