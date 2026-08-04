use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use hl_container::NetworkDriver;
use serde::Deserialize;

mod filter;
mod prune;
mod wire;

use filter::ListFilters;
use prune::Filters as PruneFilters;
use wire::Fields;

use super::{ApiError, ApiResult, DockerState};
use crate::api::{
    Network, NetworkConnect, NetworkCreate, NetworkCreated, NetworkDisconnect, NetworkPrune,
};

fn endpoint_error(error: hl_container::Error) -> ApiError {
    match error {
        hl_container::Error::AlreadyConnected { .. } | hl_container::Error::NotConnected { .. } => {
            ApiError::new(StatusCode::FORBIDDEN, error.to_string())
        }
        error => ApiError::container(error),
    }
}

#[derive(Default, Deserialize)]
pub(super) struct ListQuery {
    pub(super) filters: Option<String>,
}

#[hl_design::adapter]
pub(super) async fn list(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<Network>>> {
    let filters = ListFilters::parse(query.filters.as_deref())?;
    let networks = state
        .containers
        .networks()
        .list()
        .await
        .map_err(ApiError::container)?;
    Ok(Json(
        networks
            .into_iter()
            .filter(|network| filters.matches(network))
            .map(Network::from_summary)
            .collect(),
    ))
}

#[hl_design::adapter]
pub(super) async fn create(
    State(state): State<DockerState>,
    Json(request): Json<NetworkCreate>,
) -> ApiResult<(StatusCode, Json<NetworkCreated>)> {
    let spec = request.spec()?;
    let network = state
        .containers
        .networks()
        .create(spec)
        .await
        .map_err(ApiError::container)?;
    let mut attributes = network.labels.clone();
    attributes.insert("name".into(), network.name.clone());
    attributes.insert(
        "type".into(),
        match network.driver {
            NetworkDriver::None => "none",
            NetworkDriver::Bridge => "bridge",
        }
        .into(),
    );
    state
        .events
        .object("network", "create", network.id.to_string(), attributes);
    Ok((
        StatusCode::CREATED,
        Json(NetworkCreated {
            id: network.id.to_string(),
            warning: String::new(),
        }),
    ))
}

#[hl_design::adapter]
pub(super) async fn inspect(State(state): State<DockerState>, Path(id): Path<String>) -> ApiResult<Json<Network>> {
    state
        .containers
        .networks()
        .inspect(&id)
        .await
        .map(Network::from)
        .map(Json)
        .map_err(ApiError::container)
}

#[derive(Default, Deserialize)]
pub(super) struct RemoveQuery {
    force: Option<bool>,
}

#[hl_design::adapter]
pub(super) async fn remove(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> ApiResult<StatusCode> {
    let service = state.containers.networks();
    let network = service.inspect(&id).await.map_err(ApiError::container)?;
    if network.predefined() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "predefined networks cannot be removed",
        ));
    }
    let removed = if query.force.unwrap_or(false) {
        service.force_remove(&id).await
    } else {
        service.remove(&id).await
    };
    match removed {
        Ok(_) => {}
        Err(hl_container::Error::NetworkInUse(name)) => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                format!("network {name:?} has active endpoints"),
            ));
        }
        Err(error) => return Err(ApiError::container(error)),
    }
    let mut attributes = network.labels;
    attributes.insert("name".into(), network.name);
    attributes.insert(
        "type".into(),
        match network.driver {
            NetworkDriver::None => "none",
            NetworkDriver::Bridge => "bridge",
        }
        .into(),
    );
    state
        .events
        .object("network", "destroy", network.id.to_string(), attributes);
    Ok(StatusCode::NO_CONTENT)
}

#[hl_design::adapter]
pub(super) async fn connect(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Json(request): Json<NetworkConnect>,
) -> ApiResult<StatusCode> {
    Fields::from(&request.unsupported).reject("network connect")?;
    let spec = request.endpoint_config.unwrap_or_default().spec()?;
    state
        .containers
        .networks()
        .connect(&id, &request.container, spec)
        .await
        .map_err(endpoint_error)?;
    let network = state
        .containers
        .networks()
        .inspect(&id)
        .await
        .map_err(ApiError::container)?;
    state.events.object(
        "network",
        "connect",
        network.id.to_string(),
        [("name".into(), network.name), ("container".into(), request.container)]
            .into_iter()
            .collect(),
    );
    Ok(StatusCode::OK)
}

#[hl_design::adapter]
pub(super) async fn disconnect(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Json(request): Json<NetworkDisconnect>,
) -> ApiResult<StatusCode> {
    Fields::from(&request.unsupported).reject("network disconnect")?;
    match state.containers.networks().disconnect(&id, &request.container).await {
        Ok(_) => {
            let network = state
                .containers
                .networks()
                .inspect(&id)
                .await
                .map_err(ApiError::container)?;
            state.events.object(
                "network",
                "disconnect",
                network.id.to_string(),
                [("name".into(), network.name), ("container".into(), request.container)]
                    .into_iter()
                    .collect(),
            );
            Ok(StatusCode::OK)
        }
        Err(hl_container::Error::NotConnected { .. }) if request.force => Ok(StatusCode::OK),
        Err(error) => Err(endpoint_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use hl_container::Error;

    use super::endpoint_error;

    #[test]
    fn endpoint_state_conflicts_use_docker_forbidden_status() {
        let container: hl_container::ContainerId = "00000000000000000000000000000000".parse().unwrap();
        let connected = endpoint_error(Error::AlreadyConnected {
            container: container.clone(),
            network: "frontend".into(),
        });
        assert_eq!(connected.status, StatusCode::FORBIDDEN);

        let disconnected = endpoint_error(Error::NotConnected {
            container,
            network: "frontend".into(),
        });
        assert_eq!(disconnected.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn endpoint_lookup_and_validation_keep_shared_mapping() {
        assert_eq!(
            endpoint_error(Error::NetworkNotFound("missing".into())).status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            endpoint_error(Error::InvalidNetwork("bad endpoint".into())).status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[hl_design::adapter]
pub(super) async fn prune(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<NetworkPrune>> {
    let filters = PruneFilters::parse(query.filters.as_deref())?;
    let service = state.containers.networks();
    let mut deleted = Vec::new();
    for network in service.list().await.map_err(ApiError::container)? {
        if !network.predefined() && network.endpoints.is_empty() && filters.matches(&network) {
            match service.remove(network.id.as_str()).await {
                Ok(network) => {
                    state.events.object(
                        "network",
                        "destroy",
                        network.id.to_string(),
                        [
                            ("name".into(), network.name.clone()),
                            ("reclaimed".into(), "true".into()),
                        ]
                        .into_iter()
                        .collect(),
                    );
                    deleted.push(network.name);
                }
                Err(hl_container::Error::NetworkInUse(_) | hl_container::Error::NetworkNotFound(_)) => {}
                Err(error) => return Err(ApiError::container(error)),
            }
        }
    }
    Ok(Json(NetworkPrune {
        networks_deleted: deleted,
    }))
}
