//! `docker logs` — replay of the ordered log buffer with `--tail`/`--since`/`--until`/`--timestamps`,
//! plus `--follow` streaming from the container's `Live.out` broadcast.
use super::super::*;
use super::frame::*;

#[derive(Deserialize)]
pub(crate) struct LogsQ {
    /// `--tail`: "all" (or absent) for everything, otherwise the number of trailing lines.
    tail: Option<String>,
    /// `--timestamps`: prefix each line with an RFC3339 timestamp.
    timestamps: Option<String>,
    /// `--follow`: after replaying the buffer, keep the body open and stream new output until the
    /// container exits (or the client disconnects). A non-live/exited container just returns the buffer.
    follow: Option<String>,
    /// Stream selection. Docker requests at least one; default to both when neither is given.
    stdout: Option<String>,
    stderr: Option<String>,
    /// `--since` / `--until`: unix-timestamp filters (seconds; an optional `.nanos` suffix is dropped).
    since: Option<String>,
    until: Option<String>,
}

impl LogsQ {
    fn since(&self) -> Option<i64> {
        Logs::timestamp(self.since.as_deref())
    }
    fn until(&self) -> Option<i64> {
        Logs::timestamp(self.until.as_deref())
    }
}

pub(crate) async fn containers_logs(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<LogsQ>,
) -> Response {
    let follow = QueryFlag::is_true(&q.follow);
    let timestamps = QueryFlag::is_true(&q.timestamps);
    // Stream selection: honor explicit stdout/stderr flags, defaulting to both when neither is given.
    let (mut want_out, mut want_err) =
        (QueryFlag::is_true(&q.stdout), QueryFlag::is_true(&q.stderr));
    if !want_out && !want_err {
        want_out = true;
        want_err = true;
    }
    // `--tail`: "all"/absent/unparsable -> everything; a number -> that many trailing lines.
    let tail = match q.tail.as_deref() {
        None | Some("") | Some("all") => None,
        Some(s) => s.parse::<usize>().ok(),
    };
    let since = q.since();
    let until = q.until();

    // Persisted the container + its live IO under the daemon lock; clone the Arc<Live> so we can read its
    // buffers / subscribe to its broadcast after releasing the lock.
    let (tty, running, live, persisted_out, persisted_err, start_t, end_t) = {
        let g = a.inner.lock().await;
        let Some((full, c)) = ContainerId::get(&g, &id) else {
            return ErrorMessage::no_such(&id);
        };
        let running = c.status == "running" || c.status == "paused";
        let start_t = if c.started_at > 0 {
            c.started_at
        } else {
            c.created
        };
        let end_t = if c.finished_at > 0 {
            c.finished_at
        } else {
            now_secs()
        };
        (
            c.tty,
            running,
            g.live.get(&full).cloned(),
            c.stdout.clone(),
            c.stderr.clone(),
            start_t,
            end_t,
        )
    };

    // For follow we tail the container's COMPLETE retained `log_chunks` (the unbounded ordered log), NOT
    // the bounded `Live.out` broadcast: a slow `logs -f` client made the broadcast receiver LAG and skip
    // chunks (silent live truncation). Polling the retained log from a snapshot index instead never drops
    // output — the ordered log holds every chunk for the container's life.
    let follow_live = follow && running && live.is_some();
    // Follow ends on `out_done` (pumps fully drained), NOT the immediate `exit`, so a fast-exiting
    // guest's final lines aren't lost to the pump race (mirrors the attach/exec hijack writer).
    let exit_sub = live.as_ref().map(|l| l.out_done_rx.clone());

    // Buffered output, as the single chronological record `(emit-secs, stream, bytes)`. While the Live
    // exists (running, or exited-but-retained for non-`--rm`) we read its ordered `log_chunks` directly,
    // so stdout/stderr replay interleaved. Once the ordered log is gone (e.g. a daemon restart, where the
    // per-stream persisted buffers are also empty since they aren't serialized) we fall back to an
    // approximate stdout-then-stderr ordering from `cc.stdout`/`cc.stderr`, the best possible then.
    // `snapshot_len` records how many log_chunks the replay covered, so follow resumes exactly after it.
    let (chunks, snapshot_len): (Vec<(i64, u8, Vec<u8>)>, usize) = match &live {
        Some(l) => {
            let lc = l.log_chunks.lock().await;
            if !lc.is_empty() {
                (lc.clone(), lc.len())
            } else {
                drop(lc);
                (
                    persisted_ordered(persisted_out, persisted_err, start_t, end_t),
                    0,
                )
            }
        }
        None => (
            persisted_ordered(persisted_out, persisted_err, start_t, end_t),
            0,
        ),
    };

    // Replay: walk the ordered log. Stream selection and the `--since`/`--until` window are applied
    // PER CHUNK — each chunk carries its own emit time, so the window keeps exactly the writes inside it.
    // (Filtering after coalescing would drop/keep a whole coalesced run by its FIRST chunk's time, so a
    // busy single-stream container — where all output coalesces into one run stamped at the first write —
    // would return everything or nothing for `--since`/`--until`.) Surviving chunks are THEN coalesced
    // into adjacent same-stream runs so line-splitting sees contiguous output (a line spanning two writes
    // stays one line); each run carries its first surviving chunk's time for `--timestamps`. Then `--tail`
    // keeps the last N lines, and each line is framed (multiplexed 8-byte header for non-TTY, raw for TTY).
    let mut runs: Vec<(i64, u8, Vec<u8>)> = Vec::new();
    for (ts, stream, data) in &chunks {
        if (*stream == 1 && !want_out) || (*stream == 2 && !want_err) {
            continue;
        }
        if since.map_or(false, |s| *ts < s) || until.map_or(false, |u| *ts > u) {
            continue;
        }
        match runs.last_mut() {
            Some((_, s, buf)) if *s == *stream => buf.extend_from_slice(data),
            _ => runs.push((*ts, *stream, data.clone())),
        }
    }
    let mut entries: Vec<(i64, u8, Vec<u8>)> = Vec::new();
    for (ts, stream, data) in &runs {
        for line in Logs::new(data).lines() {
            entries.push((*ts, *stream, line));
        }
    }
    if let Some(n) = tail {
        if entries.len() > n {
            entries.drain(0..entries.len() - n);
        }
    }
    let mut replay = Vec::new();
    for (ts, stream, line) in &entries {
        replay.extend(frame_chunk(*stream, line, tty, timestamps, *ts));
    }

    // Non-follow (or nothing live to follow): serve the buffer and end, as before.
    if !follow_live {
        return replay.into_response();
    }

    // Follow: a task emits the replay, then tails the retained ordered `log_chunks` from `snapshot_len`
    // onward — the COMPLETE record, so no chunk is ever skipped for a slow client (the old broadcast path
    // dropped `Lagged` chunks). Ends when the guest has exited AND all chunks are drained, when the client
    // disconnects (channel send fails), or when a `--until` bound passes. ~50ms poll latency is invisible.
    let log_chunks = live.as_ref().unwrap().log_chunks.clone();
    let mut done_rx = exit_sub.unwrap();
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
        if !replay.is_empty() && tx.send(replay).await.is_err() {
            return;
        }
        let want = |kind: u8| (kind == 1 && want_out) || (kind == 2 && want_err);
        let mut idx = snapshot_len;
        // If the guest finished AND drained between the snapshot and here, out_done already holds true.
        let mut exited = *done_rx.borrow();
        loop {
            // Forward every chunk appended since we last looked (the retained log holds them all).
            let new: Vec<(i64, u8, Vec<u8>)> = {
                let lc = log_chunks.lock().await;
                if idx < lc.len() {
                    lc[idx..].to_vec()
                } else {
                    Vec::new()
                }
            };
            for (_, kind, data) in &new {
                idx += 1;
                if want(*kind) {
                    let f = frame_chunk(*kind, data, tty, timestamps, now_secs());
                    if tx.send(f).await.is_err() {
                        return;
                    }
                }
            }
            if matches!(until, Some(u) if now_secs() > u) {
                break;
            }
            if exited {
                // out_done fired only AFTER the pumps flushed every chunk into log_chunks, and we just
                // drained them above — nothing more will be appended, so end the stream.
                break;
            }
            tokio::select! {
                biased;
                _ = done_rx.changed() => { exited = true; }
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
    });
    let body = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|b| (Ok::<Vec<u8>, std::io::Error>(b), rx))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(Body::from_stream(body))
        .unwrap()
}
