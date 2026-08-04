use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct WaitQuery {
    condition: Option<String>,
}

pub(in super::super) async fn wait(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<WaitQuery>,
) -> ApiResult<Json<Wait>> {
    let condition = match query.condition.as_deref().unwrap_or("not-running") {
        "not-running" => hl_container::WaitCondition::NotRunning,
        "next-exit" => hl_container::WaitCondition::NextExit,
        "removed" => hl_container::WaitCondition::Removed,
        value => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported wait condition {value:?}"),
            ));
        }
    };
    let result = state
        .containers
        .wait_for(&id, condition)
        .await
        .map_err(ApiError::container)?
        .unwrap_or(ExitStatus::Code(0));
    let status_code = match result {
        ExitStatus::Code(code) => code,
        ExitStatus::Signal(signal) => 128 + signal,
        ExitStatus::Fault { status, .. } => status,
    };
    if let Ok(container) = state.containers.inspect(&id).await {
        state.events.volumes("unmount", &container);
    }
    Ok(Json(Wait {
        status_code: i64::from(status_code),
    }))
}

#[derive(Default, Deserialize)]
pub(in super::super) struct RemoveQuery {
    #[serde(default)]
    force: bool,
    #[serde(default, rename = "v")]
    volumes: bool,
}

pub(in super::super) async fn remove(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> ApiResult<StatusCode> {
    let result = if query.volumes {
        state.containers.remove_volumes(&id, query.force).await
    } else if query.force {
        state.containers.remove_force(&id).await
    } else {
        state.containers.remove(&id).await
    };
    result.map_err(ApiError::container)?;
    Ok(StatusCode::NO_CONTENT)
}
