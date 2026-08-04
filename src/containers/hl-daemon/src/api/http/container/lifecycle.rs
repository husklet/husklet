use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct WaitQuery {
    condition: Option<String>,
}

impl WaitQuery {
    fn condition(&self) -> ApiResult<hl_container::WaitCondition> {
        match self.condition.as_deref() {
            None | Some("" | "not-running") => Ok(hl_container::WaitCondition::NotRunning),
            Some("next-exit") => Ok(hl_container::WaitCondition::NextExit),
            Some("removed") => Ok(hl_container::WaitCondition::Removed),
            Some(value) => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported wait condition {value:?}"),
            )),
        }
    }
}

pub(in super::super) async fn wait(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<WaitQuery>,
) -> ApiResult<Json<Wait>> {
    let condition = query.condition()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_condition_defaults_when_omitted_or_empty() {
        for condition in [None, Some(String::new()), Some("not-running".into())] {
            assert_eq!(
                WaitQuery { condition }.condition().unwrap(),
                hl_container::WaitCondition::NotRunning
            );
        }
    }

    #[test]
    fn wait_condition_accepts_documented_values_and_rejects_unknown_values() {
        assert_eq!(
            WaitQuery {
                condition: Some("next-exit".into())
            }
            .condition()
            .unwrap(),
            hl_container::WaitCondition::NextExit
        );
        assert_eq!(
            WaitQuery {
                condition: Some("removed".into())
            }
            .condition()
            .unwrap(),
            hl_container::WaitCondition::Removed
        );
        assert_eq!(
            WaitQuery {
                condition: Some("invalid".into())
            }
            .condition()
            .unwrap_err()
            .status,
            StatusCode::BAD_REQUEST
        );
    }
}
