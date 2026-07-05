//! POST /containers/:id/attach -- hijack the connection and stream the guest's IO.
use super::*;

/// POST /containers/:id/attach -- hijack the connection and stream the guest's IO. `docker run` (no -d)
/// and `docker run -it` use this: stdout/stderr come back framed (raw in TTY mode), and the client's
/// stdin (for -i) is fed to the guest. The hijacked stream closes when the guest exits.
pub(crate) async fn containers_attach(
    State(a): State<App>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let (full, tty) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &id) else {
            return no_such(&id);
        };
        let tty = g.containers.get(&full).map(|c| c.tty).unwrap_or(false);
        (full, tty)
    };
    let live = {
        let mut g = a.inner.lock().await;
        g.live.entry(full).or_insert_with(Live::new).clone()
    };
    spawn_hijack_io(hyper::upgrade::on(req), live, tty);
    hijack_response()
}
