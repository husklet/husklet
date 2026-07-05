//! POST /containers/:id/exec -- create an exec (record the command).
use super::*;

#[derive(Deserialize)]
pub(crate) struct ExecCreateBody {
    #[serde(rename = "Cmd")]
    cmd: Option<Vec<String>>,
    #[serde(rename = "Tty")]
    tty: Option<bool>,
    #[serde(rename = "Env")]
    env: Option<Vec<String>>,
    #[serde(rename = "WorkingDir")]
    working_dir: Option<String>,
    #[serde(rename = "User")]
    user: Option<String>,
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
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
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
        return conflict(format!(
            "Container {full} is paused, unpause the container before exec"
        ));
    }
    if status != "running" {
        return conflict(format!("Container {full} is not running"));
    }
    let cmd = body.cmd.unwrap_or_default();
    if cmd.is_empty() {
        return bad_request("No exec command specified");
    }
    let exec_id = new_id(&format!("exec-{full}"));
    g.execs.insert(
        exec_id.clone(),
        Exec {
            container_id: full,
            cmd,
            tty: body.tty.unwrap_or(false),
            started: false,
            env: body.env.unwrap_or_default(),
            working_dir: body.working_dir.unwrap_or_default(),
            user: body.user.unwrap_or_default(),
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
