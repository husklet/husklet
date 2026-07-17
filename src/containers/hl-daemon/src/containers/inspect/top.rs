//! `docker top` — a single synthetic process (hl doesn't expose a guest process tree).
use super::super::*;

/// GET /containers/:id/top -- `docker top` (one synthetic process; hl doesn't expose a guest process tree).
pub(crate) async fn containers_top(State(a): State<App>, Path(id): Path<String>) -> Response {
    let g = a.inner.lock().await;
    let Some((_, c)) = resolve_get(&g, &id) else {
        return no_such(&id);
    };
    // Docker returns a 409 for `top` on a non-running container — a synthetic PID row on a stopped
    // container makes it look alive to orchestration. Only running/paused containers have a process tree.
    if c.status != "running" && c.status != "paused" {
        return conflict(format!(
            "Container {} is not running",
            &c.id[..12.min(c.id.len())]
        ));
    }
    let cmd = c.cmd.join(" ");
    Json(crate::api::ContainerTop {
        titles: vec!["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"],
        processes: vec![vec![
            "root".into(),
            "1".into(),
            "0".into(),
            "0".into(),
            "00:00".into(),
            "?".into(),
            "00:00:00".into(),
            cmd,
        ]],
    })
    .into_response()
}
