//! POST /containers/:id/exec -- create an exec (record the command).
use super::*;

#[derive(Deserialize)]
pub(crate) struct ExecCreateBody {
    #[serde(flatten)]
    process: ProcessCreateBody,
    #[serde(rename = "Privileged")]
    privileged: Option<bool>,
}

/// POST /containers/:id/exec -- create an exec (record the command). Run it with /exec/:id/start.
pub(crate) async fn exec_create(
    State(a): State<App>,
    Path(id): Path<String>,
    Json(body): Json<ExecCreateBody>,
) -> Response {
    let mut g = a.inner.lock().await;
    let Some(full) = ContainerId::resolve(&g, &id) else {
        return ErrorMessage::no_such(&id);
    };
    // `docker exec` requires a running container. Docker distinguishes two 409s: a PAUSED container gets
    // "is paused, unpause the container before exec"; anything else not-running gets "is not running".
    // Match docker's messages so the CLI surfaces them verbatim.
    let status = g
        .containers
        .get(&full)
        .map(|c| c.status.clone())
        .unwrap_or_default();
    if status == "paused" {
        return ErrorMessage::conflict(format!(
            "Container {full} is paused, unpause the container before exec"
        ));
    }
    if status != "running" {
        return ErrorMessage::conflict(format!("Container {full} is not running"));
    }
    let cmd = body.process.cmd.unwrap_or_default();
    if cmd.is_empty() {
        return ErrorMessage::bad_request("No exec command specified");
    }
    let exec_id = ContainerId::new(&format!("exec-{full}"));
    // Docker emits a container `exec_create: <cmd>` event (Actor = the parent container) so event mirrors
    // track exec lifecycle. The action carries the command, matching docker's shape.
    crate::events::emit_event(
        &a.events,
        "container",
        &format!("exec_create: {}", cmd.join(" ")),
        &full,
        json!({"execID": exec_id, "name": full}),
    );
    g.execs.insert(
        exec_id.clone(),
        Exec {
            container_id: full.clone(),
            cmd,
            tty: body.process.tty.unwrap_or(false),
            started: false,
            env: body.process.env.unwrap_or_default(),
            working_dir: body.process.working_dir.unwrap_or_default(),
            user: body.process.user.unwrap_or_default(),
            // `--privileged`: metadata only (no Linux-cap enforcement in the JIT). Accept + record it so
            // exec inspect reflects it; the spawn path is unchanged (mirrors -e/-w/-u being plain fields).
            privileged: body.privileged.unwrap_or(false),
            exit_code: 0,
        },
    );
    (
        StatusCode::CREATED,
        Json(crate::api::ExecCreateResponse { id: exec_id }),
    )
        .into_response()
}
