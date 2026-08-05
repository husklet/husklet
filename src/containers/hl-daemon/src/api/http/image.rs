use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use hl_images::format::docker::{Archive, Limits};
use hl_images::{Platform, Reference};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::DockerState;
use super::error::{ApiError, ApiResult};
use crate::api::{
    CommitOptions, Distribution, DockerError, ImageCommit, ImageDelete, ImageHistory, ImageLoad, ImagePrune,
    ImageSummary, InspectImage, PullProgress, Search,
};

const MAX_IMAGE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024 * 1024;

impl DockerState {
    async fn commit(&self, options: CommitOptions) -> ApiResult<ImageCommit> {
        if let Some(name) = options.unsupported.keys().next() {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("commit option {name:?} is not implemented"),
            ));
        }
        if options.author.contains('\0') || options.comment.contains('\0') {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "commit metadata contains NUL"));
        }
        self.containers
            .validate_commit(&options.container, &options.changes)
            .await
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        if options.container.is_empty() {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "container is required"));
        }
        if options.repo.is_empty() {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "repo is required"));
        }
        let name: Reference = format!(
            "{}:{}",
            options.repo,
            if options.tag.is_empty() { "latest" } else { &options.tag }
        )
        .parse()
        .map_err(ApiError::image_request)?;
        let container = self
            .containers
            .inspect(&options.container)
            .await
            .map_err(ApiError::container)?;
        let paused = options.pause && container.state.is_active() && !container.state.is_paused();
        if paused {
            self.containers
                .pause(&options.container)
                .await
                .map_err(ApiError::container)?;
        }
        let result = self
            .containers
            .commit(
                &options.container,
                name,
                hl_container::CommitMetadata {
                    author: (!options.author.is_empty()).then_some(options.author),
                    comment: (!options.comment.is_empty()).then_some(options.comment),
                    changes: options.changes,
                },
            )
            .await;
        let resumed = if paused {
            self.containers.unpause(&options.container).await
        } else {
            Ok(())
        };
        match (result, resumed) {
            (Ok(image), Ok(())) => {
                let images = self.containers.images().map_err(ApiError::container)?;
                let platform = self.platform.clone();
                let selected = image.clone();
                let id = tokio::task::spawn_blocking(move || images.image_id(&selected, &platform))
                    .await
                    .map_err(ApiError::task)?
                    .map_err(ApiError::image)?
                    .to_string();
                self.events.image("commit", &id, image.name.to_string());
                Ok(ImageCommit { id })
            }
            (Err(error), _) | (Ok(_), Err(error)) => Err(ApiError::container(error)),
        }
    }
    async fn image_records(&self) -> ApiResult<Vec<hl_images::Image>> {
        let images = self.containers.images().map_err(ApiError::container)?;
        tokio::task::spawn_blocking(move || images.list())
            .await
            .map_err(ApiError::task)?
            .map(|images| {
                images
                    .into_iter()
                    .filter(|image| !image.name.repository().starts_with("hl-build-cache/"))
                    .collect()
            })
            .map_err(ApiError::image)
    }

    pub(super) async fn image_summaries(&self) -> ApiResult<Vec<ImageSummary>> {
        self.image_summaries_with_shared_size(false).await
    }

    async fn image_summaries_with_shared_size(&self, include_shared_size: bool) -> ApiResult<Vec<ImageSummary>> {
        let images = self.containers.images().map_err(ApiError::container)?;
        let platform = self.platform.clone();
        tokio::task::spawn_blocking(move || {
            let inventory = images.inventory()?;
            build_image_summaries(
                inventory,
                include_shared_size,
                |target| images.size_target(target),
                |unique| images.usage_targets(unique),
                |target| images.image_id_target(target, &platform).map(|id| id.to_string()),
                |target| images.details_target(target, &platform).map(|details| details.labels),
            )
        })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)
    }

    pub(super) async fn find_image(&self, name: &str) -> ApiResult<hl_images::Image> {
        let records = self.image_records().await?;
        let images = self.containers.images().map_err(ApiError::container)?;
        let platform = self.platform.clone();
        let candidates = records.clone();
        let wanted = name.to_owned();
        let selected = tokio::task::spawn_blocking(move || {
            candidates
                .into_iter()
                .map(|image| images.image_id(&image, &platform).map(|id| (image, id)))
                .find_map(|result| match result {
                    Ok((image, id)) if id.to_string() == wanted => Some(Ok(image)),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                })
                .transpose()
        })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
        if let Some(image) = selected {
            return Ok(image.clone());
        }
        let reference: Reference = name
            .parse()
            .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("No such image: {name}")))?;
        let canonical = reference.to_string();
        records
            .into_iter()
            .find(|image| image.name.to_string() == canonical)
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("No such image: {name}")))
    }
}

fn build_image_summaries<Size, Usage, Identity, Labels>(
    inventory: Vec<hl_images::Graph>,
    include_shared_size: bool,
    mut size: Size,
    usage: Usage,
    mut identity: Identity,
    mut labels: Labels,
) -> hl_images::Result<Vec<ImageSummary>>
where
    Size: FnMut(&hl_images::Descriptor) -> hl_images::Result<u64>,
    Usage: FnOnce(&[hl_images::Descriptor]) -> hl_images::Result<BTreeMap<String, hl_images::ImageUsage>>,
    Identity: FnMut(&hl_images::Descriptor) -> hl_images::Result<String>,
    Labels: FnMut(&hl_images::Descriptor) -> hl_images::Result<BTreeMap<String, String>>,
{
    let mut grouped = BTreeMap::<String, (hl_images::Descriptor, Vec<String>)>::new();
    for graph in inventory {
        if graph.build_cache {
            continue;
        }
        let tags = graph
            .names
            .into_iter()
            .map(|name| name.parse::<Reference>())
            .collect::<hl_images::Result<Vec<_>>>()?
            .into_iter()
            .filter(|name| !name.repository().starts_with("hl-build-cache/"))
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        let id = identity(&graph.target)?;
        match grouped.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((graph.target, tags));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => entry.get_mut().1.extend(tags),
        }
    }
    let unique = grouped.values().map(|(target, _)| target.clone()).collect::<Vec<_>>();
    let usage = include_shared_size.then(|| usage(&unique)).transpose()?;
    grouped
        .into_iter()
        .map(|(id, (target, mut repo_tags))| {
            repo_tags.sort();
            let (size_bytes, shared_size) = match &usage {
                Some(usage) => {
                    let target_id = target.digest().to_string();
                    let usage = usage.get(&target_id).copied().ok_or_else(|| {
                        hl_images::Error::InvalidMetadata(format!("image usage is missing target {target_id}"))
                    })?;
                    (usage.size, i64::try_from(usage.shared).unwrap_or(i64::MAX))
                }
                None => (size(&target)?, -1),
            };
            let size = i64::try_from(size_bytes).unwrap_or(i64::MAX);
            Ok(ImageSummary {
                id,
                repo_tags,
                repo_digests: Vec::new(),
                created: 0,
                size,
                shared_size,
                virtual_size: size,
                labels: labels(&target)?,
                containers: -1,
            })
        })
        .collect()
}

mod archive;
mod fields;
mod identity;
mod list;
mod registry;

pub(super) use archive::{load, save};
use fields::{Field, Fields};
#[cfg(test)]
use identity::{RemoveQuery, removal_conflicts};
#[cfg(test)]
use list::{ImageSelection, ListQuery};
pub(super) use list::{Prune, list, prune};
pub(super) use registry::{commit, distribution, pull, search};

/// Dispatches named-image operations whose repository reference contains path separators.
///
/// Axum's single-segment `:name` routes own short names and image IDs. Docker sends
/// repository-qualified names as literal path segments, so this terminal wildcard preserves the
/// complete reference and separates only the operation suffix.
pub(super) async fn named(
    State(state): State<DockerState>,
    Path(path): Path<String>,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = request.headers().clone();
    match method {
        Method::GET => {
            if let Some(name) = path.strip_suffix("/json").filter(|name| !name.is_empty()) {
                return identity::inspect(State(state), Path(name.to_owned())).await.into_response();
            }
            if let Some(name) = path.strip_suffix("/history").filter(|name| !name.is_empty()) {
                return identity::history(State(state), Path(name.to_owned())).await.into_response();
            }
        }
        Method::POST => {
            if let Some(name) = path.strip_suffix("/tag").filter(|name| !name.is_empty()) {
                return match Query::<identity::TagQuery>::try_from_uri(&uri) {
                    Ok(query) => identity::tag(State(state), Path(name.to_owned()), query).await.into_response(),
                    Err(rejection) => rejection.into_response(),
                };
            }
            if let Some(name) = path.strip_suffix("/push").filter(|name| !name.is_empty()) {
                return match Query::<super::push::Options>::try_from_uri(&uri) {
                    Ok(query) => super::push::post(State(state), Path(name.to_owned()), query, headers).await,
                    Err(rejection) => rejection.into_response(),
                };
            }
        }
        Method::DELETE if !path.is_empty() => {
            return match Query::<identity::RemoveQuery>::try_from_uri(&uri) {
                Ok(query) => identity::remove(State(state), Path(path), query).await.into_response(),
                Err(rejection) => rejection.into_response(),
            };
        }
        _ => {}
    }
    StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
mod tests;
