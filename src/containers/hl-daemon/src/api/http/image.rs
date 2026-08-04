use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
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
                let id = image.target.digest().to_string();
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
                |target| images.details_target(target, &platform).map(|details| details.labels),
            )
        })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)
    }

    pub(super) async fn find_image(&self, name: &str) -> ApiResult<hl_images::Image> {
        let records = self.image_records().await?;
        if let Some(image) = records.iter().find(|image| image.target.digest().to_string() == name) {
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

fn build_image_summaries<Size, Usage, Labels>(
    inventory: Vec<hl_images::Graph>,
    include_shared_size: bool,
    mut size: Size,
    usage: Usage,
    mut labels: Labels,
) -> hl_images::Result<Vec<ImageSummary>>
where
    Size: FnMut(&hl_images::Descriptor) -> hl_images::Result<u64>,
    Usage: FnOnce(&[hl_images::Descriptor]) -> hl_images::Result<BTreeMap<String, hl_images::ImageUsage>>,
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
        let id = graph.target.digest().to_string();
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
                    let usage = usage.get(&id).copied().ok_or_else(|| {
                        hl_images::Error::InvalidMetadata(format!("image usage is missing target {id}"))
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
pub(super) use identity::{history, inspect, remove, tag};
#[cfg(test)]
use list::{ImageSelection, ListQuery};
pub(super) use list::{Prune, list, prune};
pub(super) use registry::{commit, distribution, pull, search};

#[cfg(test)]
mod tests;
