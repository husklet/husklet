//! POST /containers/:id/resize and /exec/:id/resize -- set the PTY window size.
use super::*;

#[derive(Deserialize)]
pub(crate) struct ResizeQ {
    h: Option<u16>,
    w: Option<u16>,
}

/// POST /containers/:id/resize and /exec/:id/resize -- set the PTY window size (TIOCSWINSZ) for a tty
/// container/exec. Always 200 so `docker run -t` never prints "failed to resize tty".
pub(crate) async fn resize(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<ResizeQ>,
) -> Response {
    let g = a.inner.lock().await;
    let key = resolve_cid(&g, &id).unwrap_or(id);
    if let Some(live) = g.live.get(&key) {
        if let Some(fd) = *live.pty_master.lock().unwrap() {
            let ws = libc::winsize {
                ws_row: q.h.unwrap_or(24),
                ws_col: q.w.unwrap_or(80),
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe {
                libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
            }
        }
    }
    StatusCode::OK.into_response()
}
