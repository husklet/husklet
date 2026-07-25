use super::{
    ApiError, ApiResult, AsyncWriteExt, BTreeMap, Body, CommitOptions, Deserialize, Distribution,
    DockerError, DockerState, Fields, HeaderMap, ImageCommit, ImageLoad, IntoResponse, Json, Path,
    Platform, PullProgress, Query, Reference, Response, Search, Seek, SeekFrom, State, StatusCode,
    MAX_IMAGE_ARCHIVE_BYTES,
};

#[hl_design::adapter]
pub(in super::super) async fn commit(
    State(state): State<DockerState>,
    Query(options): Query<CommitOptions>,
) -> ApiResult<(StatusCode, Json<ImageCommit>)> {
    Ok((StatusCode::CREATED, Json(state.commit(options).await?)))
}

#[derive(Default, Deserialize)]
pub(in super::super) struct SearchQuery {
    term: String,
    limit: Option<usize>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

#[hl_design::adapter]
pub(in super::super) async fn search(
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Vec<Search>>> {
    Fields::from(&query.unsupported).reject("image search")?;
    if query.term.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "term is required"));
    }
    let _ = query.limit;
    Ok(Json(Vec::new()))
}

#[hl_design::adapter]
pub(in super::super) async fn distribution(
    State(state): State<DockerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Distribution>> {
    let image = state.find_image(&name).await.map_err(|error| {
        if error.status == StatusCode::NOT_FOUND {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("No such distribution: {name}"),
            )
        } else {
            error
        }
    })?;
    Ok(Json(Distribution {
        descriptor: image.target,
        platforms: vec![state.platform],
    }))
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in super::super) struct PullQuery {
    from_image: Option<String>,
    from_src: Option<String>,
    repo: Option<String>,
    tag: Option<String>,
    platform: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

pub(in super::super) async fn pull(
    State(state): State<DockerState>,
    Query(query): Query<PullQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Response> {
    Fields::from(&query.unsupported).reject("image create")?;
    if query
        .from_src
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return state.import(query, body).await;
    }
    let auth = headers
        .get("x-registry-auth")
        .map(|value| {
            let value = value.to_str().map_err(|_| {
                ApiError::new(StatusCode::BAD_REQUEST, "invalid X-Registry-Auth header")
            })?;
            crate::api::Credentials::decode(value)
                .and_then(crate::api::Credentials::auth)
                .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))
        })
        .transpose()?
        .unwrap_or_default();
    let from_image = query
        .from_image
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "fromImage is required"))?;
    let mut reference: Reference = from_image.parse().map_err(ApiError::image_request)?;
    if let Some(tag) = query.tag.filter(|value| !value.is_empty()) {
        if tag.contains(['/', '@']) {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid image tag"));
        }
        reference = format!("{}/{}:{tag}", reference.registry(), reference.repository())
            .parse()
            .map_err(ApiError::image_request)?;
    }
    let platform = query
        .platform
        .as_deref()
        .map(|value| {
            value
                .parse::<Platform>()
                .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))
        })
        .transpose()?
        .unwrap_or_else(|| state.platform.clone());
    let images = state.containers.images().map_err(ApiError::container)?;
    let source: std::sync::Arc<dyn hl_images::remote::Source> = match auth {
        hl_images::remote::Auth::Anonymous => state.source.clone(),
        auth if reference.registry().starts_with("127.0.0.1:")
            || reference.registry().starts_with("localhost:") =>
        {
            std::sync::Arc::new(hl_images::remote::Registry::insecure(auth))
        }
        auth => std::sync::Arc::new(hl_images::remote::Registry::new(auth)),
    };
    let display = reference.to_string();
    let events = state.events.clone();
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(2);
    tokio::spawn(async move {
        let _ = sender
            .send(Ok(PullProgress {
                status: Some(format!("Pulling from {}", reference.repository())),
                id: Some(display.clone()),
                ..PullProgress::default()
            }
            .bytes()))
            .await;
        let progress = match images.pull(source.as_ref(), reference, &platform).await {
            Ok(image) => {
                events.image(
                    "pull",
                    image.target.digest().to_string(),
                    image.name.to_string(),
                );
                PullProgress {
                    status: Some(format!("Status: downloaded newer image for {}", image.name)),
                    id: Some(image.target.digest().to_string()),
                    ..PullProgress::default()
                }
            }
            Err(error) => {
                let message = error.to_string();
                PullProgress {
                    error: Some(message.clone()),
                    error_detail: Some(DockerError { message }),
                    ..PullProgress::default()
                }
            }
        };
        let _ = sender.send(Ok(progress.bytes())).await;
    });
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Body::from_stream(futures_util::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) },
        )),
    )
        .into_response())
}

impl DockerState {
    async fn import(self, query: PullQuery, body: Body) -> ApiResult<Response> {
        let repository = query
            .repo
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "repo is required"))?;
        let reference: Reference = query
            .tag
            .filter(|value| !value.is_empty())
            .map_or(repository.clone(), |tag| format!("{repository}:{tag}"))
            .parse()
            .map_err(ApiError::image_request)?;
        let mut stream = body.into_data_stream();
        let mut layer = tokio::fs::File::from_std(tempfile::tempfile().map_err(ApiError::io)?);
        let mut received = 0_u64;
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            let chunk =
                chunk.map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
            received = received.saturating_add(chunk.len() as u64);
            if received > MAX_IMAGE_ARCHIVE_BYTES {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "image archive exceeds 128 GiB",
                ));
            }
            layer.write_all(&chunk).await.map_err(ApiError::io)?;
        }
        layer.flush().await.map_err(ApiError::io)?;
        let mut layer = layer.into_std().await;
        layer.seek(SeekFrom::Start(0)).map_err(ApiError::io)?;
        let images = self.containers.images().map_err(ApiError::container)?;
        let platform = self.platform;
        let display = reference.to_string();
        tokio::task::spawn_blocking(move || {
            images.import(
                layer,
                &hl_images::RuntimeConfig {
                    entrypoint: Vec::new(),
                    command: vec!["/bin/sh".into()],
                    environment: BTreeMap::new(),
                    working_directory: "/".into(),
                    user: String::new(),
                },
                &platform,
                &reference,
            )
        })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image_request)?;
        Ok(Json(ImageLoad {
            stream: format!("Loaded image: {display}\n"),
        })
        .into_response())
    }
}
