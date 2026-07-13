//! GET /exec/:id/json -- exec inspect (Running / ExitCode).
use super::*;

/// GET /exec/:id/json -- exec inspect (Running / ExitCode), how the CLI learns the exec's result.
pub(crate) async fn exec_inspect(State(a): State<App>, Path(id): Path<String>) -> Response {
    let g = a.inner.lock().await;
    let Some(exec) = g.execs.get(&id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("no such exec: {id}")})),
        )
            .into_response();
    };
    // While the exec is live, read Running/ExitCode from its Live's exit watch. Once it exits the reaper
    // drops the Live (freeing its buffers) but records the code on the Exec, so fall back to that here.
    let (running, code, pid) = match g.live.get(&id) {
        Some(l) => match *l.exit_rx.borrow() {
            Some(c) => (false, c, 0),
            None => (true, 0, l.pid.lock().unwrap().unwrap_or(0) as i64),
        },
        None => (false, exec.exit_code, 0),
    };
    Json(crate::api::ExecInspect {
        id,
        running,
        exit_code: code,
        container_id: exec.container_id,
        process_config: crate::api::ExecProcessConfig {
            tty: exec.tty,
            privileged: exec.privileged,
            entrypoint: exec.cmd.first().cloned().unwrap_or_default(),
            arguments: exec.cmd.get(1..).map(|s| s.to_vec()).unwrap_or_default(),
        },
        // dd streams all three exec channels; CanRemove is false (dd auto-reaps exec Live on exit).
        open_stdin: exec.tty,
        open_stdout: true,
        open_stderr: true,
        can_remove: false,
        detach_keys: String::new(),
        pid,
    })
    .into_response()
}
