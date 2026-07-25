use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hl_images::format::docker::{Archive, Limits};
use hl_images::{Platform, Reference};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::error::{ApiError, ApiResult};
use super::DockerState;
use crate::api::{
    CommitOptions, Distribution, DockerError, ImageCommit, ImageDelete, ImageHistory, ImageLoad,
    ImagePrune, ImageSummary, InspectImage, PullProgress, Search,
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
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "commit metadata contains NUL",
            ));
        }
        self.containers
            .validate_commit(&options.container, &options.changes)
            .await
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        if options.container.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "container is required",
            ));
        }
        if options.repo.is_empty() {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "repo is required"));
        }
        let name: Reference = format!(
            "{}:{}",
            options.repo,
            if options.tag.is_empty() {
                "latest"
            } else {
                &options.tag
            }
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

    async fn image_summaries_with_shared_size(
        &self,
        include_shared_size: bool,
    ) -> ApiResult<Vec<ImageSummary>> {
        let mut grouped: BTreeMap<String, (i64, Vec<String>)> = BTreeMap::new();
        let records = self.image_records().await?;
        let images = self.containers.images().map_err(ApiError::container)?;
        let platform = self.platform.clone();
        let records = tokio::task::spawn_blocking(move || {
            let usage = images.usage(&records)?;
            records
                .into_iter()
                .map(|image| {
                    let id = image.target.digest().to_string();
                    let usage = usage.get(&id).copied().ok_or_else(|| {
                        hl_images::Error::InvalidMetadata(format!(
                            "image usage is missing target {id}"
                        ))
                    })?;
                    let labels = images.details(&image, &platform)?.labels;
                    Ok::<_, hl_images::Error>((image, usage, labels))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
        let mut labels_by_id = BTreeMap::new();
        let mut shared_by_id = BTreeMap::new();
        for (image, usage, labels) in records {
            let id = image.target.digest().to_string();
            labels_by_id.insert(id.clone(), labels);
            let size = i64::try_from(usage.size).unwrap_or(i64::MAX);
            shared_by_id.insert(id.clone(), i64::try_from(usage.shared).unwrap_or(i64::MAX));
            grouped
                .entry(id)
                .and_modify(|(_, tags)| tags.push(image.name.to_string()))
                .or_insert_with(|| (size, vec![image.name.to_string()]));
        }
        Ok(grouped
            .into_iter()
            .map(|(id, (size, mut repo_tags))| {
                repo_tags.sort();
                let labels = labels_by_id.remove(&id).unwrap_or_default();
                let shared_size = if include_shared_size {
                    shared_by_id.remove(&id).unwrap_or_default()
                } else {
                    -1
                };
                ImageSummary {
                    id,
                    repo_tags,
                    repo_digests: Vec::new(),
                    created: 0,
                    size,
                    shared_size,
                    virtual_size: size,
                    labels,
                    containers: -1,
                }
            })
            .collect())
    }

    pub(super) async fn find_image(&self, name: &str) -> ApiResult<hl_images::Image> {
        let records = self.image_records().await?;
        if let Some(image) = records
            .iter()
            .find(|image| image.target.digest().to_string() == name)
        {
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

mod archive;
mod fields;
mod identity;
mod list;
mod registry;

pub(super) use archive::{load, save};
use fields::{Field, Fields};
#[cfg(test)]
use identity::RemoveQuery;
pub(super) use identity::{history, inspect, remove, tag};
pub(super) use list::{list, prune, Prune};
#[cfg(test)]
use list::{ImageSelection, ListQuery};
pub(super) use registry::{commit, distribution, pull, search};

#[cfg(test)]
mod tests;
