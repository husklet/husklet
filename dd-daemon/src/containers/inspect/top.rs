//! `docker top` — a single synthetic process (dd doesn't expose a guest process tree).
use super::super::*;

/// GET /containers/:id/top -- `docker top` (one synthetic process; dd doesn't expose a guest process tree).
pub(crate) async fn containers_top(State(a): State<App>, Path(id): Path<String>) -> Response {
    let g = a.inner.lock().await;
    let Some((_, c)) = resolve_get(&g, &id) else {
        return no_such(&id);
    };
    let cmd = c.cmd.join(" ");
    Json(crate::api::ContainerTop {
        titles: vec![
            "UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD",
        ],
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
