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
    let include_size = flag(query.size.as_deref());
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
    inspect.metadata.mounts = super::mount::points(&state.containers, &container.spec.mounts).await?;
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
