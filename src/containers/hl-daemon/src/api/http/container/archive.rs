use super::*;

#[derive(Deserialize)]
pub(in super::super) struct ArchiveQuery {
    pub(super) path: String,
    #[serde(default, rename = "copyUIDGID")]
    pub(super) copy_uid_gid: bool,
    #[serde(default, rename = "noOverwriteDirNonDir")]
    pub(super) no_overwrite_dir_non_dir: bool,
}

impl ArchiveQuery {
    pub(super) fn extraction(&self) -> hl_container::Extraction {
        hl_container::Extraction {
            copy_uid_gid: self.copy_uid_gid,
            no_overwrite_dir_non_dir: self.no_overwrite_dir_non_dir,
        }
    }
}

pub(in super::super) async fn stat(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    let filesystem = state.containers.filesystem(&id).await.map_err(ApiError::container)?;
    let stat = tokio::task::spawn_blocking(move || filesystem.stat(query.path))
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::container)?;
    let header = PathStat::from(stat)
        .header()
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok((StatusCode::OK, [("X-Docker-Container-Path-Stat", header)]).into_response())
}

pub(in super::super) async fn archive(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    let filesystem = state.containers.filesystem(&id).await.map_err(ApiError::container)?;
    let stat = filesystem.stat(&query.path).map_err(ApiError::container)?;
    let header = PathStat::from(stat)
        .header()
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let file = tempfile::tempfile().map_err(ApiError::io)?;
    let path = query.path;
    let file = tokio::task::spawn_blocking(move || {
        let mut file = file;
        filesystem.archive(path, &mut file)?;
        file.seek(SeekFrom::Start(0))?;
        Ok::<_, hl_container::Error>(file)
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::container)?;
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file)));
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE.as_str(), "application/x-tar".into()),
            ("X-Docker-Container-Path-Stat", header),
        ],
        body,
    )
        .into_response())
}

#[hl_design::adapter]
pub(in super::super) async fn export(state: State<DockerState>, id: Path<String>) -> ApiResult<Response> {
    archive(
        state,
        id,
        Query(ArchiveQuery {
            path: "/".to_owned(),
            copy_uid_gid: false,
            no_overwrite_dir_non_dir: false,
        }),
    )
    .await
}

pub(in super::super) async fn extract(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<ArchiveQuery>,
    mut body: Body,
) -> ApiResult<StatusCode> {
    let extraction = query.extraction();
    let filesystem = state.containers.filesystem(&id).await.map_err(ApiError::container)?;
    let mut file = tokio::fs::File::from_std(tempfile::tempfile().map_err(ApiError::io)?);
    let mut received = 0_u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        if let Ok(data) = frame.into_data() {
            received = received.saturating_add(data.len() as u64);
            if received > ARCHIVE_LIMIT {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "container archive exceeds upload limit",
                ));
            }
            file.write_all(&data).await.map_err(ApiError::io)?;
        }
    }
    file.flush().await.map_err(ApiError::io)?;
    file.seek(SeekFrom::Start(0)).await.map_err(ApiError::io)?;
    let file = file.into_std().await;
    tokio::task::spawn_blocking(move || {
        filesystem.extract_with(
            query.path,
            file,
            hl_container::Limits {
                entries: 100_000,
                bytes: ARCHIVE_LIMIT,
            },
            extraction,
        )
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::container)?;
    Ok(StatusCode::OK)
}
