use super::{
    ApiError, ApiResult, BTreeMap, Deserialize, DockerState, Field, Fields, ImageDelete, ImageHistory, InspectImage,
    Json, Path, Query, Reference, State, StatusCode,
};

#[hl_design::adapter]
pub(in super::super) async fn inspect(
    State(state): State<DockerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<InspectImage>> {
    let selected = state.find_image(&name).await?;
    let size = i64::try_from(selected.target.size()).unwrap_or(i64::MAX);
    let images = state.containers.images().map_err(ApiError::container)?;
    let image = selected.clone();
    let platform = state.platform.clone();
    let (id, details) = tokio::task::spawn_blocking(move || {
        Ok::<_, hl_images::Error>((
            images.image_id(&image, &platform)?.to_string(),
            images.details(&image, &platform)?,
        ))
    })
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
    let (id, details) = tokio::task::spawn_blocking(move || {
        Ok::<_, hl_images::Error>((
            images.image_id(&image, &platform)?.to_string(),
            images.details(&image, &platform)?,
        ))
    })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
    let size = i64::try_from(selected.target.size()).unwrap_or(i64::MAX);
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
    let target: Reference = value
        .parse()
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, format!("invalid image reference: {error}")))?;
    let images = state.containers.images().map_err(ApiError::container)?;
    let platform = state.platform.clone();
    let source_id = tokio::task::spawn_blocking(move || {
        let id = images.image_id(&source, &platform)?.to_string();
        images.tag(&source, target)?;
        Ok::<_, hl_images::Error>(id)
    })
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

pub(super) fn removal_conflicts(
    force: bool,
    selected_by_id: bool,
    alias_count: usize,
    references: impl IntoIterator<Item = bool>,
) -> bool {
    let mut any = false;
    let mut active = false;
    for reference_is_active in references {
        any = true;
        active |= reference_is_active;
    }
    if selected_by_id && alias_count > 1 && !force {
        return true;
    }
    let removes_target = force || selected_by_id || alias_count <= 1;
    removes_target && (active || (any && !force))
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
    let identity_images = images.clone();
    let identity_image = image.clone();
    let platform = state.platform.clone();
    let image_id = tokio::task::spawn_blocking(move || identity_images.image_id(&identity_image, &platform))
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?
        .to_string();
    let aliases = state
        .image_records()
        .await?
        .into_iter()
        .filter(|candidate| candidate.target.digest() == image.target.digest())
        .map(|candidate| candidate.name.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let target_digest = image.target.digest().to_string();
    let containers = state.containers.list().await.map_err(ApiError::container)?;
    let references = containers.iter().filter_map(|container| {
        container.spec.image.as_ref().and_then(|reference| {
            let reference = reference.to_string();
            (reference == image_id || aliases.contains(&reference)).then(|| container.state.is_active())
        })
    });
    if removal_conflicts(force, name == image_id, aliases.len(), references) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("conflict: image {name} is being used by a container"),
        ));
    }
    let reference = image.name.clone();
    let removed = tokio::task::spawn_blocking(move || {
        if force {
            images.force_remove(&image)
        } else {
            images.remove(&reference).map(|image| image.into_iter().collect())
        }
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image)?;
    if removed.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, format!("No such image: {name}")));
    }
    for image in &removed {
        state.events.image("untag", &image_id, image.name.to_string());
    }
    let images = state.containers.images().map_err(ApiError::container)?;
    let digest_for_lookup = target_digest.clone();
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
        let images = state.containers.images().map_err(ApiError::container)?;
        let selected = std::collections::BTreeSet::from([target_digest]);
        tokio::task::spawn_blocking(move || images.prune_graphs(&selected))
            .await
            .map_err(ApiError::task)?
            .map_err(ApiError::image)?;
        state.events.image("delete", &image_id, removed[0].name.to_string());
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
            deleted: Some(image_id),
        });
    }
    Ok(Json(response))
}
