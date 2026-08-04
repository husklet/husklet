use axum::Json;
use axum::extract::{OriginalUri, Query, State};
use axum::http::StatusCode;
use hl_container::ContainerState;
use hl_images::content::Store;

use super::{ApiError, ApiResult, DockerState};
use crate::api::{
    Authentication, BuildCache, Container, Credentials, ImageSummary, Plugin, SystemInfo, SystemPrune, UsageData,
    Version, VolumeUsage,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[hl_design::adapter]
pub(super) async fn ping() -> &'static str {
    "OK"
}

#[hl_design::adapter]
pub(super) async fn version(State(state): State<DockerState>) -> Json<Version> {
    Json(Version {
        version: state.release.version,
        api_version: "1.43".into(),
        min_api_version: "1.24".into(),
        os: state.platform.os,
        arch: state.platform.architecture,
    })
}

#[hl_design::adapter]
pub(super) async fn plugins() -> Json<Vec<Plugin>> {
    Json(Vec::new())
}

#[hl_design::adapter]
pub(super) async fn auth(Json(_credentials): Json<Credentials>) -> Json<Authentication> {
    Json(Authentication {
        status: "Login Succeeded".into(),
        identity_token: String::new(),
    })
}

#[hl_design::adapter]
pub(super) async fn info(State(state): State<DockerState>) -> ApiResult<Json<SystemInfo>> {
    let containers = state.containers.list().await.map_err(ApiError::container)?;
    let running = containers
        .iter()
        .filter(|container| matches!(container.state, ContainerState::Running { .. }))
        .count();
    let paused = containers
        .iter()
        .filter(|container| container.state.is_paused())
        .count();
    let images = state.image_summaries().await?;
    Ok(Json(SystemInfo {
        id: "hl-daemon".into(),
        containers: i64::try_from(containers.len()).unwrap_or(i64::MAX),
        containers_running: i64::try_from(running).unwrap_or(i64::MAX),
        containers_paused: i64::try_from(paused).unwrap_or(i64::MAX),
        containers_stopped: i64::try_from(containers.len().saturating_sub(running + paused)).unwrap_or(i64::MAX),
        images: i64::try_from(images.len()).unwrap_or(i64::MAX),
        driver: "hl-engine".into(),
        memory_limit: false,
        ncpu: std::thread::available_parallelism().map_or(1, |value| i64::try_from(value.get()).unwrap_or(i64::MAX)),
        os_type: state.platform.os.clone(),
        architecture: state.platform.architecture.clone(),
        operating_system: "Linux".into(),
        name: "hl-daemon".into(),
        server_version: state.release.version,
    }))
}

#[derive(Clone, Copy, Default)]
struct DiskSelection {
    containers: bool,
    images: bool,
    volumes: bool,
    build_cache: bool,
}

impl DiskSelection {
    const fn all() -> Self {
        Self {
            containers: true,
            images: true,
            volumes: true,
            build_cache: true,
        }
    }

    fn from_pairs(parameters: Vec<(String, String)>) -> ApiResult<Self> {
        let mut selection = Self::default();
        let mut specified = false;
        for (name, value) in parameters {
            if name != "type" {
                continue;
            }
            specified = true;
            match value.as_str() {
                "container" => selection.containers = true,
                "image" => selection.images = true,
                "volume" => selection.volumes = true,
                "build-cache" => selection.build_cache = true,
                _ => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        format!("unknown object type: {value}"),
                    ));
                }
            }
        }
        if !specified {
            return Ok(Self::all());
        }
        Ok(selection)
    }

    fn for_request(uri: &axum::http::Uri, parameters: Vec<(String, String)>) -> ApiResult<Self> {
        let minor = uri
            .path()
            .strip_prefix("/v1.")
            .and_then(|path| path.split('/').next())
            .and_then(|minor| minor.parse::<u16>().ok());
        if minor.is_some_and(|minor| minor < 42) {
            return Ok(Self::all());
        }
        Self::from_pairs(parameters)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct DiskUsageResponse {
    layers_size: i64,
    images: Option<Vec<ImageSummary>>,
    containers: Option<Vec<Container>>,
    volumes: Option<Vec<VolumeUsage>>,
    build_cache: Option<Vec<BuildCache>>,
}

#[hl_design::adapter]
pub(super) async fn disk(
    State(state): State<DockerState>,
    OriginalUri(uri): OriginalUri,
    Query(parameters): Query<Vec<(String, String)>>,
) -> ApiResult<Json<DiskUsageResponse>> {
    let selection = DiskSelection::for_request(&uri, parameters)?;
    let (containers, images, volumes) = tokio::join!(
        selected_containers(state.clone(), selection.containers),
        selected_images(state.clone(), selection.images),
        selected_volumes(state, selection.volumes),
    );
    let containers = containers?;
    let images = images?;
    let volumes = volumes?;
    let (layers_size, images) = images.map_or((0, None), |(size, images)| (size, Some(images)));
    Ok(Json(DiskUsageResponse {
        layers_size,
        images,
        containers,
        volumes,
        build_cache: selection.build_cache.then(Vec::new),
    }))
}

async fn selected_containers(state: DockerState, selected: bool) -> ApiResult<Option<Vec<Container>>> {
    if !selected {
        return Ok(None);
    }
    let listed_containers = state.containers.list().await.map_err(ApiError::container)?;
    let containers = super::container::summaries(&state.containers, listed_containers, true).await?;
    Ok(Some(containers))
}

async fn selected_images(state: DockerState, selected: bool) -> ApiResult<Option<(i64, Vec<ImageSummary>)>> {
    if !selected {
        return Ok(None);
    }
    let image_state = state.clone();
    let (images, layers_size) = tokio::join!(state.image_summaries(), layers_size(image_state));
    Ok(Some((layers_size?, images?)))
}

async fn selected_volumes(state: DockerState, selected: bool) -> ApiResult<Option<Vec<VolumeUsage>>> {
    if !selected {
        return Ok(None);
    }
    let volume_service = state.containers.volumes();
    let listed_volumes = volume_service.list().await.map_err(ApiError::container)?;
    let reference_counts = volume_service
        .reference_counts(&listed_volumes)
        .await
        .map_err(ApiError::container)?;
    let mut sizes = volume_service
        .sizes(&listed_volumes)
        .await
        .map_err(ApiError::container)?;
    let mut volumes = Vec::new();
    for volume in listed_volumes {
        let size = sizes.remove(&volume.name).ok_or_else(|| {
            ApiError::container(hl_container::Error::Corrupt(format!(
                "volume size result omitted {:?}",
                volume.name
            )))
        })?;
        let references = reference_counts.get(&volume.name).copied().unwrap_or_default();
        volumes.push(VolumeUsage {
            name: volume.name,
            mountpoint: volume.path.to_string_lossy().into_owned(),
            usage_data: UsageData {
                size: i64::try_from(size).unwrap_or(i64::MAX),
                ref_count: i64::try_from(references).unwrap_or(i64::MAX),
            },
        });
    }
    Ok(Some(volumes))
}

async fn layers_size(state: DockerState) -> ApiResult<i64> {
    let image_service = state.containers.images().map_err(ApiError::container)?;
    tokio::task::spawn_blocking(move || {
        image_service
            .content()
            .digests()?
            .into_iter()
            .try_fold(0_i64, |total, digest| {
                let size = image_service.content().info(&digest)?.size;
                Ok::<_, hl_images::Error>(total.saturating_add(i64::try_from(size).unwrap_or(i64::MAX)))
            })
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image)
}

#[derive(Default, Deserialize)]
pub(super) struct PruneQuery {
    #[serde(default)]
    volumes: bool,
    filters: Option<String>,
}

#[hl_design::adapter]
pub(super) async fn prune(
    State(state): State<DockerState>,
    Query(query): Query<PruneQuery>,
) -> ApiResult<Json<SystemPrune>> {
    let filters = Filters::parse(query.filters.as_deref())?;
    // Compute and validate every resource projection before the first deletion. Docker's `until`
    // filter applies to timestamped resources, while volumes receive only the label selection.
    let common = filters.raw(&["until", "label", "label!"])?;
    crate::api::filter::Prune::parse(common.as_deref())
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;
    let volume_filters = filters.raw(&["label", "label!"])?;
    let image_filters = common.clone();
    let Json(containers) = super::container::prune(
        State(state.clone()),
        Query(super::container::PruneQuery {
            filters: common.clone(),
        }),
    )
    .await?;
    let Json(networks) = super::network::prune(
        State(state.clone()),
        Query(super::network::ListQuery { filters: common }),
    )
    .await?;
    let volumes = if query.volumes {
        super::volume::prune(
            State(state.clone()),
            Query(super::volume::ListQuery {
                filters: Filters::with_all(volume_filters.as_deref())?,
            }),
        )
        .await?
        .0
    } else {
        crate::api::VolumePrune {
            volumes_deleted: Vec::new(),
            space_reclaimed: 0,
        }
    };
    let images = super::image::Prune::parse(image_filters.as_deref())?
        .execute(&state)
        .await?;
    let image_space = u64::try_from(images.space_reclaimed).unwrap_or(0);
    let result = SystemPrune {
        containers_deleted: containers.containers_deleted,
        images_deleted: images.images_deleted,
        networks_deleted: networks.networks_deleted,
        volumes_deleted: volumes.volumes_deleted,
        space_reclaimed: containers
            .space_reclaimed
            .saturating_add(volumes.space_reclaimed)
            .saturating_add(image_space),
    };
    hl_log::hl_info!(
        hl_log::tag::DAEMON,
        "system prune containers={} images={} networks={} volumes={} reclaimed={}",
        result.containers_deleted.len(),
        result.images_deleted.len(),
        result.networks_deleted.len(),
        result.volumes_deleted.len(),
        result.space_reclaimed
    );
    Ok(Json(result))
}

#[derive(Default)]
struct Filters(BTreeMap<String, Vec<String>>);

impl Filters {
    fn parse(raw: Option<&str>) -> ApiResult<Self> {
        let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
            return Ok(Self::default());
        };
        let values: BTreeMap<String, Vec<String>> = serde_json::from_str(raw).map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid system prune filters: {error}"),
            )
        })?;
        let unsupported = values
            .keys()
            .filter(|name| !matches!(name.as_str(), "until" | "label" | "label!"))
            .cloned()
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported system prune filters: {}", unsupported.join(", ")),
            ));
        }
        Ok(Self(values))
    }

    fn raw(&self, names: &[&str]) -> ApiResult<Option<String>> {
        let values = self
            .0
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, values)| (name.clone(), values.clone()))
            .collect::<BTreeMap<_, _>>();
        (!values.is_empty())
            .then(|| serde_json::to_string(&values))
            .transpose()
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))
    }

    fn with_all(raw: Option<&str>) -> ApiResult<Option<String>> {
        let mut values: BTreeMap<String, Vec<String>> = raw
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?
            .unwrap_or_default();
        values.insert("all".into(), vec!["true".into()]);
        serde_json::to_string(&values)
            .map(Some)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))
    }
}
