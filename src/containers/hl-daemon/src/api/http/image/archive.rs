use super::{
    ApiError, ApiResult, Archive, AsyncWriteExt, BTreeMap, Body, Deserialize, DockerState, Field, Fields, ImageLoad,
    IntoResponse, Json, Limits, MAX_IMAGE_ARCHIVE_BYTES, Query, ReaderStream, Response, Seek, SeekFrom, State,
    StatusCode,
};

#[derive(Default, Deserialize)]
pub(in super::super) struct LoadQuery {
    quiet: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

pub(in super::super) async fn load(
    State(state): State<DockerState>,
    Query(query): Query<LoadQuery>,
    body: Body,
) -> ApiResult<Json<ImageLoad>> {
    Fields::from(&query.unsupported).reject("image load")?;
    let quiet = Field::new("quiet", query.quiet.as_deref()).boolean()?;
    let mut file = tokio::fs::File::from_std(tempfile::tempfile().map_err(ApiError::io)?);
    let mut received = 0_u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        received = received.saturating_add(chunk.len() as u64);
        if received > MAX_IMAGE_ARCHIVE_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image archive exceeds 128 GiB",
            ));
        }
        file.write_all(&chunk).await.map_err(ApiError::io)?;
    }
    file.flush().await.map_err(ApiError::io)?;
    let mut file = file.into_std().await;
    let images = state.containers.images().map_err(ApiError::container)?;
    let imported = tokio::task::spawn_blocking(move || {
        file.seek(SeekFrom::Start(0))?;
        Archive::load(file, &images, Limits::default())
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image_request)?;
    let names = imported
        .into_iter()
        .map(|image| image.name.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Json(ImageLoad {
        stream: if quiet {
            String::new()
        } else {
            format!("Loaded image: {names}\n")
        },
    }))
}

#[derive(Default, Deserialize)]
pub(in super::super) struct SaveQuery {
    names: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

#[hl_design::adapter]
pub(in super::super) async fn save(
    State(state): State<DockerState>,
    Query(query): Query<SaveQuery>,
) -> ApiResult<Response> {
    Fields::from(&query.unsupported).reject("image save")?;
    let records = state.image_records().await?;
    let names: Vec<_> = query
        .names
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .collect();
    let selected = if names.is_empty() {
        records
    } else {
        let mut selected = Vec::with_capacity(names.len());
        for name in names {
            selected.push(state.find_image(name).await?);
        }
        selected
    };
    let images = state.containers.images().map_err(ApiError::container)?;
    let file = tokio::task::spawn_blocking(move || {
        let mut file = tempfile::tempfile()?;
        Archive::save(&mut file, &images, &selected)?;
        file.seek(SeekFrom::Start(0))?;
        Ok::<_, hl_images::Error>(file)
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image)?;
    let stream = ReaderStream::new(tokio::fs::File::from_std(file));
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/x-tar")],
        Body::from_stream(stream),
    )
        .into_response())
}
