use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct InspectQuery {
    size: Option<String>,
}

pub(in super::super) async fn inspect(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<InspectQuery>,
) -> ApiResult<Json<InspectContainer>> {
    let include_size = bool::from(query.size.as_deref().unwrap_or_default().parse::<Flag>()?);
    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    let container_id = container.id.clone();
    let network_mode = container.spec.network_mode;
    let mut inspect = InspectContainer::from(container.clone());
    if include_size {
        let usage = state
            .containers
            .filesystem_usage(&id)
            .await
            .map_err(ApiError::container)?;
        inspect.size(usage);
    }
    for mount in container.spec.mounts {
        let (kind, name, source, driver) = match mount.source {
            hl_container::MountSource::Bind(source) => (
                "bind",
                String::new(),
                source.to_string_lossy().into_owned(),
                String::new(),
            ),
            hl_container::MountSource::Volume(name) | hl_container::MountSource::Anonymous(name) => {
                let volume = state
                    .containers
                    .volumes()
                    .inspect(&name)
                    .await
                    .map_err(ApiError::container)?;
                (
                    "volume",
                    name,
                    volume.path.to_string_lossy().into_owned(),
                    "local".into(),
                )
            }
            hl_container::MountSource::Tmpfs(name) => {
                let volume = state
                    .containers
                    .volumes()
                    .inspect(&name)
                    .await
                    .map_err(ApiError::container)?;
                (
                    "tmpfs",
                    String::new(),
                    volume.path.to_string_lossy().into_owned(),
                    String::new(),
                )
            }
        };
        let read_write = mount.access == hl_container::Access::ReadWrite;
        inspect.metadata.mounts.push(MountPoint {
            kind: kind.into(),
            name,
            source,
            destination: mount.target.to_string_lossy().into_owned(),
            driver,
            mode: if read_write { "rw" } else { "ro" }.into(),
            read_write,
            propagation: match mount.propagation {
                hl_container::BindPropagation::Private => "private",
                hl_container::BindPropagation::RecursivePrivate => "rprivate",
            }
            .into(),
        });
    }
    if network_mode != hl_container::NetworkMode::Host {
        for network in state.containers.networks().list().await.map_err(ApiError::container)? {
            let Some(endpoint) = network.endpoints.get(&container_id) else {
                continue;
            };
            inspect.host_config.network_mode.clone_from(&network.name);
            let settings = crate::api::EndpointSettings::from((&network, endpoint));
            inspect.network_settings.networks.insert(network.name, settings);
        }
    }
    Ok(Json(inspect))
}

#[hl_design::adapter]
pub(in super::super) async fn changes(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Change>>> {
    let changes = state
        .containers
        .changes(&id)
        .await
        .map_err(ApiError::container)?
        .into_inner()
        .into_iter()
        .map(Change::from)
        .collect();
    Ok(Json(changes))
}

pub(in super::super) async fn update(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Json(request): Json<Update>,
) -> ApiResult<Json<UpdateResult>> {
    let settings = request
        .settings()
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;
    state
        .containers
        .update(&id, settings)
        .await
        .map_err(ApiError::container)?;
    Ok(Json(UpdateResult::default()))
}
