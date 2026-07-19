//! POST /containers/:id/resize and /exec/:id/resize -- set the PTY window size.
use super::*;

#[derive(Deserialize)]
pub(crate) struct ResizeQ {
    h: Option<u16>,
    w: Option<u16>,
}

/// Whether a resize target — a container (by id/name/prefix) OR an exec id — exists at all. A resize for
/// a truly-missing target must 404; a resolvable-but-not-yet-live one still 200s (its pty may not be
/// ready, and `docker run -t` must not see a spurious failure).
impl Execs {
    pub(crate) fn target_exists(g: &Inner, id: &str) -> bool {
        ContainerId::resolve(g, id).is_some() || g.execs.contains_key(id)
    }
}

/// POST /containers/:id/resize and /exec/:id/resize -- set the PTY window size (TIOCSWINSZ) for a tty
/// container/exec. 200 for a live/known target (so `docker run -t` never prints "failed to resize tty"),
/// 404 for a target that doesn't exist.
pub(crate) async fn resize(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<ResizeQ>,
) -> Response {
    let g = a.inner.lock().await;
    if !Execs::target_exists(&g, &id) {
        return ErrorMessage::no_such(&id);
    }
    let key = ContainerId::resolve(&g, &id).unwrap_or(id);
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

#[cfg(test)]
mod tests {
    use crate::containers::Execs;
    use crate::model::{Container, Exec, Inner};

    // "Resize Missing Container Or Exec Reports Success" (P2): a resize for a nonexistent target must be
    // recognized as missing (-> 404), while a real container or exec is a valid target.
    #[test]
    fn resize_target_exists_for_container_and_exec_only() {
        let mut g = Inner::default();
        g.containers.insert(
            "c1full".into(),
            Container {
                id: "c1full".into(),
                name: "web".into(),
                ..Default::default()
            },
        );
        g.execs.insert(
            "exec1".into(),
            Exec {
                container_id: "c1full".into(),
                cmd: vec![],
                tty: false,
                started: false,
                env: vec![],
                working_dir: String::new(),
                user: String::new(),
                privileged: false,
                exit_code: 0,
            },
        );
        assert!(Execs::target_exists(&g, "web"), "container by name");
        assert!(Execs::target_exists(&g, "c1full"), "container by id");
        assert!(Execs::target_exists(&g, "exec1"), "exec by id");
        assert!(
            !Execs::target_exists(&g, "ghost"),
            "unknown target is missing"
        );
    }
}
