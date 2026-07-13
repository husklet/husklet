//! POST /containers/:id/attach -- hijack the connection and stream the guest's IO.
use super::*;

/// POST /containers/:id/attach -- hijack the connection and stream the guest's IO. `docker run` (no -d)
/// and `docker run -it` use this: stdout/stderr come back framed (raw in TTY mode), and the client's
/// stdin (for -i) is fed to the guest. The hijacked stream closes when the guest exits.
/// Whether `docker attach` must be refused. An exited/dead container with no retained live IO cannot
/// deliver process output, so upgrading the connection to a hijack stream would hang the client forever;
/// Docker returns a conflict ("You cannot attach to a stopped container"). A `created` container (the
/// `docker run` create→attach→start flow) legitimately has no live yet, so it is allowed — start fills it.
pub(crate) fn attach_conflicts(status: &str, has_live: bool) -> bool {
    !has_live && matches!(status, "exited" | "dead")
}

/// `docker attach` stream selectors (`?stdin=1&stdout=1&stderr=1`). When NONE are set docker defaults to
/// stdout+stderr (attach without `-i`); an explicit selector narrows to exactly the requested streams.
#[derive(Deserialize, Default)]
pub(crate) struct AttachQ {
    stdin: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
    #[allow(dead_code)] // `stream=`/`logs=` are accepted for wire-compat; dd always streams live.
    stream: Option<String>,
    #[allow(dead_code)]
    logs: Option<String>,
}

pub(crate) async fn containers_attach(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<AttachQ>,
    req: Request,
) -> Response {
    // Resolve the requested streams. If the client set none, default to stdout+stderr (docker's default
    // attach); otherwise honor exactly the selected ones.
    let streams = if q.stdin.is_none() && q.stdout.is_none() && q.stderr.is_none() {
        HijackStreams { stdin: false, stdout: true, stderr: true }
    } else {
        HijackStreams {
            stdin: q_truthy(&q.stdin),
            stdout: q_truthy(&q.stdout),
            stderr: q_truthy(&q.stderr),
        }
    };
    let (full, tty, status) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &id) else {
            return no_such(&id);
        };
        let (tty, status) = g
            .containers
            .get(&full)
            .map(|c| (c.tty, c.status.clone()))
            .unwrap_or((false, String::new()));
        (full, tty, status)
    };
    let live = {
        let mut g = a.inner.lock().await;
        if attach_conflicts(&status, g.live.contains_key(&full)) {
            return conflict(format!("container {id} is not running"));
        }
        g.live.entry(full).or_insert_with(Live::new).clone()
    };
    spawn_hijack_io_sel(hyper::upgrade::on(req), live, tty, streams);
    hijack_response()
}

#[cfg(test)]
mod tests {
    use super::attach_conflicts;

    // "Attach Exited Container Without Live State Creates Hijack" (P1): attaching to an exited/dead
    // container that has no retained live IO must be refused (409), not upgraded to a dead hijack stream.
    #[test]
    fn attach_rejected_only_for_stopped_without_live() {
        assert!(attach_conflicts("exited", false), "exited + no live -> reject");
        assert!(attach_conflicts("dead", false), "dead + no live -> reject");
        // A running/paused container, or one that still has retained live IO, is attachable.
        assert!(!attach_conflicts("running", false));
        assert!(!attach_conflicts("paused", false));
        assert!(!attach_conflicts("exited", true), "exited but live IO retained -> allowed");
        // `created` (the run create->attach->start flow) has no live yet but must be allowed.
        assert!(!attach_conflicts("created", false), "created must be attachable");
    }
}
