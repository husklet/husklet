use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct ListQuery {
    #[serde(default)]
    all: bool,
    filters: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct NetworkAttachment {
    pub(super) name: String,
    endpoint: hl_container::EndpointSpec,
}

#[derive(Clone, Debug)]
pub(super) struct NetworkPlan {
    pub(super) attachments: Vec<NetworkAttachment>,
    built_in: Option<NetworkDriver>,
    isolated: bool,
    mode: hl_container::NetworkMode,
}

impl NetworkPlan {
    pub(super) fn from_request(
        host: Option<&HostConfig>,
        config: Option<&NetworkingConfig>,
    ) -> ApiResult<Self> {
        let mode = host.map_or("", |host| host.network_mode.as_str());
        let endpoints = config
            .map(|config| config.endpoints_config.0.clone())
            .unwrap_or_default();
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
                built_in: None,
                isolated: false,
                mode: hl_container::NetworkMode::Host,
            });
        }
        if mode == "none"
            && !endpoints.is_empty()
            && (endpoints.len() > 1 || !endpoints.contains_key("none"))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "none NetworkMode only accepts the none endpoint",
            ));
        }
        if !matches!(mode, "" | "default" | "bridge" | "none")
            && !endpoints.is_empty()
            && !endpoints.contains_key(mode)
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
        let (endpoints, built_in) = if endpoints.is_empty() {
            let name = match mode {
                "" | "default" | "bridge" => DEFAULT_NETWORK,
                "none" => "none",
                name => name,
            };
            let built_in = match name {
                DEFAULT_NETWORK => Some(NetworkDriver::Bridge),
                "none" => Some(NetworkDriver::None),
                _ => None,
            };
            (
                BTreeMap::from([(name.to_owned(), EndpointConfig::default())]),
                built_in,
            )
        } else {
            (endpoints, None)
        };
        let attachments = endpoints
            .into_iter()
            .map(|(name, endpoint)| {
                endpoint
                    .spec()
                    .map(|endpoint| NetworkAttachment { name, endpoint })
            })
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(Self {
            attachments,
            built_in,
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

    pub(super) async fn prepare(
        mut self,
        containers: &hl_container::Containers,
    ) -> ApiResult<Self> {
        if self.mode == hl_container::NetworkMode::Host {
            return Ok(self);
        }
        match self.built_in {
            Some(NetworkDriver::None) => Self::ensure_none(containers).await?,
            Some(NetworkDriver::Bridge) => Self::ensure_bridge(containers).await?,
            None => {}
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

    pub(super) async fn attach_created(
        self,
        containers: &hl_container::Containers,
        container: &str,
    ) -> ApiResult<()> {
        if self.attachments.is_empty() {
            return Ok(());
        }
        let requests = self
            .attachments
            .into_iter()
            .map(|attachment| (attachment.name, attachment.endpoint));
        if let Err(error) = containers
            .networks()
            .connect_many(container, requests)
            .await
        {
            let error = ApiError::container(error);
            let cleanup = containers.remove_volumes(container, false).await;
            return match cleanup {
                Ok(_) => Err(error),
                Err(cleanup) => Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "network attachment failed ({error:?}); container rollback also failed ({cleanup})"
                    ),
                )),
            };
        }
        Ok(())
    }

    async fn ensure_none(containers: &hl_container::Containers) -> ApiResult<()> {
        containers
            .networks()
            .create(NetworkSpec::none("none"))
            .await
            .map(|_| ())
            .map_err(ApiError::container)
    }

    pub(super) async fn ensure_bridge(containers: &hl_container::Containers) -> ApiResult<()> {
        match containers.networks().inspect(DEFAULT_NETWORK).await {
            Ok(network) if network.driver == NetworkDriver::Bridge => return Ok(()),
            Ok(_) => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "default bridge name belongs to another network driver",
                ))
            }
            Err(ContainerError::NetworkNotFound(_)) => {}
            Err(error) => return Err(ApiError::container(error)),
        }
        for second in [31_u8, 30, 29, 28] {
            for third in 0_u8..=255 {
                let subnet = Subnet::new(std::net::Ipv4Addr::new(172, second, third, 0), 24)
                    .map_err(ApiError::container)?;
                match containers
                    .networks()
                    .create(NetworkSpec::bridge(DEFAULT_NETWORK, subnet))
                    .await
                {
                    Ok(_) => return Ok(()),
                    Err(ContainerError::InvalidNetwork(message))
                        if message.contains("overlaps") => {}
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
                            .map_err(ApiError::container)
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
    let selection = crate::api::List::parse(query.all, query.filters.as_deref())
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))?;
    let values = state.containers.list().await.map_err(ApiError::container)?;
    let values = values
        .iter()
        .filter(|value| selection.includes_inactive() || value.state.is_active())
        .filter(|value| selection.matches_in(value, &values))
        .cloned()
        .map(Container::from)
        .collect();
    Ok(Json(values))
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
        containers_deleted: removed
            .into_iter()
            .map(|container| container.id.to_string())
            .collect(),
        space_reclaimed: 0,
    }))
}
