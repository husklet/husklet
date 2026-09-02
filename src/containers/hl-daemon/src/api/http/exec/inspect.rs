use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hl_container::{ExecState, ExitStatus};

use super::super::DockerState;
use super::super::error::{ApiError, ApiResult};
use crate::api::{ExecInspect, ExecOpen, ExecProcess};

#[hl_design::adapter]
pub(in crate::api::http) async fn inspect(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ExecInspect>> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let exec = state
        .containers
        .executions()
        .inspect(&exec_id)
        .await
        .map_err(ApiError::container)?;
    model(exec)
}

pub(super) fn model(exec: hl_container::Exec) -> ApiResult<Json<ExecInspect>> {
    let Inspection {
        running,
        exit_code,
        pid,
    } = exec.state.try_into()?;
    let process = exec.spec.process;
    Ok(Json(ExecInspect {
        id: exec.id.to_string(),
        container_id: exec.container.to_string(),
        running,
        exit_code,
        pid,
        can_remove: !running,
        detach_keys: exec.spec.detach_keys,
        open: ExecOpen {
            stdin: exec.spec.streams.stdin,
            stdout: exec.spec.streams.stdout,
            stderr: exec.spec.streams.stderr,
        },
        process: ExecProcess {
            arguments: process.args,
            entrypoint: process.program,
            privileged: exec.spec.privileged,
            tty: process.console.terminal.is_some(),
            user: exec.spec.user,
        },
    }))
}

#[derive(Debug, Eq, PartialEq)]
struct Inspection {
    running: bool,
    exit_code: i64,
    pid: i64,
}

impl TryFrom<ExecState> for Inspection {
    type Error = ApiError;

    fn try_from(state: ExecState) -> Result<Self, Self::Error> {
        let (running, exit_code, pid) = match state {
            ExecState::Created => (false, 0, 0),
            ExecState::Running { process_id, .. } => (
                true,
                0,
                i64::try_from(process_id)
                    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "exec PID exceeds i64"))?,
            ),
            ExecState::Exited { result, process_id, .. } => {
                let code = match result {
                    ExitStatus::Code(code) => code,
                    ExitStatus::Signal(signal) => 128 + signal,
                    ExitStatus::Fault { status, .. } => status,
                };
                let pid = process_id
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "exec PID exceeds i64"))?
                    .unwrap_or_default();
                (false, i64::from(code), pid)
            }
        };
        Ok(Self {
            running,
            exit_code,
            pid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecState, ExitStatus, Inspection, StatusCode};

    #[test]
    fn exited_identity() {
        assert_eq!(
            Inspection::try_from(ExecState::Exited {
                result: ExitStatus::Code(7),
                finished_at_ms: 20,
                process_id: Some(42),
            })
            .unwrap(),
            Inspection {
                running: false,
                exit_code: 7,
                pid: 42,
            }
        );
    }

    #[test]
    fn legacy_identity() {
        assert_eq!(
            Inspection::try_from(ExecState::Exited {
                result: ExitStatus::Code(0),
                finished_at_ms: 20,
                process_id: None,
            })
            .unwrap(),
            Inspection {
                running: false,
                exit_code: 0,
                pid: 0,
            }
        );
    }

    #[test]
    fn pid_bounds() {
        let error = Inspection::try_from(ExecState::Exited {
            result: ExitStatus::Code(0),
            finished_at_ms: 20,
            process_id: Some(u64::MAX),
        })
        .unwrap_err();

        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
