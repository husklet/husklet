use super::{
    ApiError, ApiResult, BTreeMap, Deserialize, DockerState, Field, Fields, ImageDelete,
    ImageHistory, InspectImage, Json, Path, Query, Reference, State, StatusCode,
};

#[hl_design::adapter]
pub(in super::super) async fn inspect(
    State(state): State<DockerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<InspectImage>> {
    let selected = state.find_image(&name).await?;
    let id = selected.target.digest().to_string();
    let size = i64::try_from(selected.target.size()).unwrap_or(i64::MAX);
    let images = state.containers.images().map_err(ApiError::container)?;
    let image = selected.clone();
    let platform = state.platform.clone();
    let details = tokio::task::spawn_blocking(move || images.details(&image, &platform))
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
    let mut repo_tags: Vec<_> = state
        .image_records()
        .await?
        .into_iter()
        .filter(|image| image.target.digest() == selected.target.digest())
        .map(|image| image.name.to_string())
        .collect();
    repo_tags.sort();
    Ok(Json(InspectImage {
        id,
        repo_tags,
        repo_digests: Vec::new(),
        created: details.created.unwrap_or_default(),
        size,
        virtual_size: size,
        os: details.platform.os,
        architecture: details.platform.architecture,
        config: crate::api::ImageConfig {
            entrypoint: details.runtime.entrypoint,
            cmd: details.runtime.command,
            env: details
                .runtime
                .environment
                .into_iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect(),
            working_dir: details.runtime.working_directory,
            user: details.runtime.user,
            labels: details.labels,
            onbuild: details.onbuild,
            exposed_ports: details
                .exposed_ports
                .into_iter()
                .map(|port| (port, serde_json::json!({})))
                .collect(),
            volumes: details
                .volumes
                .into_iter()
                .map(|path| (path, serde_json::json!({})))
                .collect(),
            healthcheck: details.healthcheck,
            stop_signal: details.stop_signal,
        },
    }))
}

#[hl_design::adapter]
pub(in super::super) async fn history(
    State(state): State<DockerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<ImageHistory>>> {
    let selected = state.find_image(&name).await?;
    let images = state.containers.images().map_err(ApiError::container)?;
    let image = selected.clone();
    let platform = state.platform.clone();
    let details = tokio::task::spawn_blocking(move || images.details(&image, &platform))
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
    let size = i64::try_from(selected.target.size()).unwrap_or(i64::MAX);
    let id = selected.target.digest().to_string();
    let tags = state
        .image_records()
        .await?
        .into_iter()
        .filter(|image| image.target.digest() == selected.target.digest())
        .map(|image| image.name.to_string())
        .collect();
    let mut entries: Vec<_> = details
        .history
        .into_iter()
        .rev()
        .map(|entry| ImageHistory {
            id: id.clone(),
            created: entry
                .created
                .as_deref()
                .and_then(|value| {
                    chrono::DateTime::parse_from_rfc3339(value)
                        .ok()
                        .map(|value| value.timestamp())
                })
                .unwrap_or_default(),
            created_by: entry.created_by.unwrap_or_default(),
            tags: Vec::new(),
            size: if entry.empty_layer { 0 } else { size },
            comment: entry.comment.unwrap_or_default(),
        })
        .collect();
    if let Some(entry) = entries.first_mut() {
        entry.tags = tags;
    }
    Ok(Json(entries))
}

#[derive(Deserialize)]
pub(in super::super) struct TagQuery {
    repo: String,
    tag: Option<String>,
    #[serde(flatten)]
    pub(super) unsupported: BTreeMap<String, String>,
}

pub(in super::super) async fn tag(
    State(state): State<DockerState>,
    Path(name): Path<String>,
    Query(query): Query<TagQuery>,
) -> ApiResult<StatusCode> {
    Fields::from(&query.unsupported).reject("image tag")?;
    let source = state.find_image(&name).await?;
    let value = query
        .tag
        .map_or(query.repo.clone(), |tag| format!("{}:{tag}", query.repo));
    let target: Reference = value.parse().map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid image reference: {error}"),
        )
    })?;
    let source_id = source.target.digest().to_string();
    let images = state.containers.images().map_err(ApiError::container)?;
    tokio::task::spawn_blocking(move || images.tag(&source, target))
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
    state.events.image("tag", source_id, value);
    Ok(StatusCode::CREATED)
}

#[derive(Default, Deserialize)]
pub(in super::super) struct RemoveQuery {
    pub(super) force: Option<String>,
    pub(super) noprune: Option<String>,
    #[serde(flatten)]
    pub(super) unsupported: BTreeMap<String, String>,
}

impl RemoveQuery {
    pub(super) fn validate(&self) -> ApiResult<bool> {
        Fields::from(&self.unsupported).reject("image remove")?;
        let force = Field::new("force", self.force.as_deref()).boolean()?;
        let _ = Field::new("noprune", self.noprune.as_deref()).boolean()?;
        Ok(force)
    }
}

pub(in super::super) async fn remove(
    State(state): State<DockerState>,
    Path(name): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> ApiResult<Json<Vec<ImageDelete>>> {
    let force = query.validate()?;
    // Removal currently untags without pruning parent layers, which exactly honors noprune=true.
    let image = state.find_image(&name).await?;
    let images = state.containers.images().map_err(ApiError::container)?;
    let reference = image.name.clone();
    let removed = tokio::task::spawn_blocking(move || {
        if force {
            images.force_remove(&image)
        } else {
            images
                .remove(&reference)
                .map(|image| image.into_iter().collect())
        }
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image)?;
    if removed.is_empty() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("No such image: {name}"),
        ));
    }
    for image in &removed {
        state.events.image(
            "untag",
            image.target.digest().to_string(),
            image.name.to_string(),
        );
    }
    let digest = removed[0].target.digest().to_string();
    let images = state.containers.images().map_err(ApiError::container)?;
    let digest_for_lookup = digest.clone();
    let retained = tokio::task::spawn_blocking(move || {
        images.list().map(|images| {
            images
                .iter()
                .any(|image| image.target.digest().to_string() == digest_for_lookup)
        })
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image)?;
    if !retained {
        state
            .events
            .image("delete", digest.clone(), removed[0].name.to_string());
    }
    let mut response = removed
        .into_iter()
        .map(|image| ImageDelete {
            untagged: Some(image.name.to_string()),
            deleted: None,
        })
        .collect::<Vec<_>>();
    if !retained {
        response.push(ImageDelete {
            untagged: None,
            deleted: Some(digest),
        });
    }
    Ok(Json(response))
}
