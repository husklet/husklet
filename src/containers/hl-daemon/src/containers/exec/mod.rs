//! Hijacked-stream IO + exec handlers: attach, the docker `/exec` create/start/inspect
//! flow, and the shared PTY resize endpoint. Moved verbatim from the former
//! `containers.rs`; shared types/helpers come from `mod.rs` via `use super::*`.
//!
//! One file per operation:
//!   - `attach`  — POST /containers/:id/attach (hijack the connection)
//!   - `create`  — POST /containers/:id/exec (record the command)
//!   - `start`   — POST /exec/:id/start (run + hijack/detach)
//!   - `inspect` — GET  /exec/:id/json (Running / ExitCode)
//!   - `resize`  — POST /containers/:id/resize + /exec/:id/resize (TIOCSWINSZ)
//! The shared hijack IO helpers (`spawn_hijack_io`, `hijack_response`) live here as they
//! are used by both attach and exec-start.
use super::*;

mod attach;
mod create;
mod inspect;
mod resize;
mod start;

pub(crate) use attach::*;
pub(crate) use create::*;
pub(crate) use inspect::*;
pub(crate) use resize::*;
pub(crate) use start::*;

/// Drive a hijacked docker stream against a Live: fan guest stdout/stderr to the client (docker
/// multiplexed frames, or raw bytes in TTY mode) and feed the client's stdin into the guest. Shared by
/// container attach and exec. `rx` is subscribed synchronously so no output is missed if the guest
/// starts producing before the upgrade completes.
/// Which streams a hijack (`docker attach`) should carry. `docker attach` accepts `stdin`/`stdout`/
/// `stderr` query selectors; a client may attach to only one. Defaults to all three (the exec path and
/// `docker run` want everything).
#[derive(Clone, Copy)]
pub(crate) struct HijackStreams {
    pub stdin: bool,
    pub stdout: bool,
    pub stderr: bool,
}

impl Default for HijackStreams {
    fn default() -> Self {
        HijackStreams {
            stdin: true,
            stdout: true,
            stderr: true,
        }
    }
}

pub(crate) fn spawn_hijack_io(on_upgrade: hyper::upgrade::OnUpgrade, live: Arc<Live>, tty: bool) {
    spawn_hijack_io_sel(on_upgrade, live, tty, HijackStreams::default())
}

/// Like [`spawn_hijack_io`] but honors per-stream selectors: output frames for a deselected stream are
/// dropped, and client stdin is fed to the guest only when `stdin` is selected.
pub(crate) fn spawn_hijack_io_sel(
    on_upgrade: hyper::upgrade::OnUpgrade,
    live: Arc<Live>,
    tty: bool,
    streams: HijackStreams,
) {
    let mut rx = live.out.subscribe();
    // Close on `out_done` (set once the pumps have flushed ALL output), NOT on the immediate `exit`:
    // `exit` fires the instant the guest dies, while its final bytes may still be in-flight in the pump
    // tasks -- breaking on `exit` raced the pumps and dropped a fast-exiting command's last output.
    let mut out_done_rx = live.out_done_rx.clone();
    let live_in = live.clone();
    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let (mut rd, mut wr) = tokio::io::split(TokioIo::new(upgraded));
        let writer = tokio::spawn(async move {
            // The guest may have already exited (and been fully drained) before the upgrade completed --
            // e.g. attaching to a retained, exited container -- so check the flag before blocking.
            let mut done = *out_done_rx.borrow();
            loop {
                // Whether an output frame for stream `kind` (1=stdout, 2=stderr) was selected by the client.
                let want = |kind: u8| {
                    if kind == 2 {
                        streams.stderr
                    } else {
                        streams.stdout
                    }
                };
                if done {
                    // Output is complete: every byte is buffered in `out`. Flush it all, then end.
                    while let Ok((kind, chunk)) = rx.try_recv() {
                        if !want(kind) {
                            continue;
                        }
                        let f = if tty { chunk } else { log_frame(kind, &chunk) };
                        let _ = wr.write_all(&f).await;
                    }
                    break;
                }
                tokio::select! {
                    biased;
                    m = rx.recv() => match m {
                        Ok((kind, chunk)) => {
                            if !want(kind) { continue; }
                            let f = if tty { chunk } else { log_frame(kind, &chunk) };
                            if wr.write_all(&f).await.is_err() { return; }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = out_done_rx.changed() => { done = true; }
                }
            }
            let _ = wr.flush().await;
            let _ = wr.shutdown().await;
        });
        // Feed client stdin to the guest only when the `stdin` stream was selected; otherwise leave the
        // guest's stdin untouched (a stdout-only attach must not block on or forward client input).
        if streams.stdin {
            let mut buf = [0u8; 8192];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if live_in.stdin_tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = live_in.stdin_tx.send(Vec::new()).await; // EOF -> close guest stdin
        }
        let _ = writer.await;
    });
}

pub(crate) const HIJACK_HEADERS: [(&str, &str); 3] = [
    ("Content-Type", "application/vnd.docker.raw-stream"),
    ("Connection", "Upgrade"),
    ("Upgrade", "tcp"),
];

pub(crate) fn hijack_response() -> Response {
    let mut b = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    for (k, v) in HIJACK_HEADERS {
        b = b.header(k, v);
    }
    b.body(Body::empty()).unwrap()
}
