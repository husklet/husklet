//! `docker logs` — replay of the ordered log buffer with `--tail`/`--since`/`--until`/`--timestamps`,
//! plus `--follow` streaming from the container's `Live.out` broadcast.
use super::super::*;

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

/// Parse a docker `since`/`until` value into unix seconds. Docker may send `"<secs>"` or
/// `"<secs>.<nanos>"`; we keep the integer seconds. Returns None for absent/0/unparsable.
fn parse_unix_ts(s: &Option<String>) -> Option<i64> {
    let v = s.as_deref().filter(|x| !x.is_empty())?;
    v.split('.')
        .next()
        .unwrap_or(v)
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
}

/// Split a log buffer into newline-terminated lines, keeping the trailing `\n` on each line and any
/// final unterminated fragment as its own line. Used to apply `--tail` and `--timestamps` per line.
fn split_log_lines(buf: &[u8]) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            lines.push(buf[start..=i].to_vec());
            start = i + 1;
        }
    }
    if start < buf.len() {
        lines.push(buf[start..].to_vec());
    }
    lines
}

/// Frame a chunk of guest output for the docker logs wire. TTY containers stream raw bytes (no demux
/// header); non-TTY uses Docker's multiplexed framing (8-byte header, stream id 1=stdout / 2=stderr).
/// With `--timestamps` the chunk is split into lines and each gets an RFC3339 prefix using `ts_secs`
/// (the chunk's recorded emit time for buffered replay, or the current time for live follow) so the
/// stamp survives demuxing exactly as dockerd writes it.
fn frame_chunk(stream: u8, data: &[u8], tty: bool, timestamps: bool, ts_secs: i64) -> Vec<u8> {
    if !timestamps {
        return if tty {
            data.to_vec()
        } else {
            log_frame(stream, data)
        };
    }
    let ts = fmt_rfc3339(ts_secs);
    let mut out = Vec::new();
    for line in split_log_lines(data) {
        let mut p = Vec::with_capacity(ts.len() + 1 + line.len());
        p.extend_from_slice(ts.as_bytes());
        p.push(b' ');
        p.extend_from_slice(&line);
        if tty {
            out.extend_from_slice(&p);
        } else {
            out.extend(log_frame(stream, &p));
        }
    }
    out
}

/// Fallback ordered log built from the per-stream persisted snapshots (`cc.stdout`/`cc.stderr`) for when
/// the Live's chronological `log_chunks` is gone (daemon restart). Without per-chunk times the true
/// interleave is unrecoverable, so we emit stdout then stderr, stamped with the run's start/finish time
/// so `--since`/`--until`/`--timestamps` still behave as they did before the ordered log existed.
fn persisted_ordered(
    out: Vec<u8>,
    err: Vec<u8>,
    start_t: i64,
    end_t: i64,
) -> Vec<(i64, u8, Vec<u8>)> {
    let mut v = Vec::new();
    if !out.is_empty() {
        v.push((start_t, 1u8, out));
    }
    if !err.is_empty() {
        v.push((end_t, 2u8, err));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unix_ts_variants() {
        // Plain seconds.
        assert_eq!(parse_unix_ts(&Some("1700000000".into())), Some(1_700_000_000));
        // `.nanos` suffix is dropped, integer seconds kept.
        assert_eq!(parse_unix_ts(&Some("1700000000.5".into())), Some(1_700_000_000));
        // Absent / empty / zero / unparsable -> None.
        assert_eq!(parse_unix_ts(&None), None);
        assert_eq!(parse_unix_ts(&Some("".into())), None);
        assert_eq!(parse_unix_ts(&Some("0".into())), None);
        assert_eq!(parse_unix_ts(&Some("notanumber".into())), None);
    }

    #[test]
    fn split_log_lines_keeps_trailing_newline_and_fragment() {
        // Each complete line keeps its `\n`; a final unterminated fragment is its own line.
        assert_eq!(
            split_log_lines(b"a\nbb\nc"),
            vec![b"a\n".to_vec(), b"bb\n".to_vec(), b"c".to_vec()]
        );
    }

    #[test]
    fn split_log_lines_all_terminated_has_no_trailing_fragment() {
        assert_eq!(
            split_log_lines(b"x\ny\n"),
            vec![b"x\n".to_vec(), b"y\n".to_vec()]
        );
    }

    #[test]
    fn split_log_lines_empty_is_empty() {
        assert!(split_log_lines(b"").is_empty());
    }

    #[test]
    fn frame_chunk_tty_no_timestamps_is_raw() {
        // TTY streams raw bytes with no demux header when timestamps are off.
        assert_eq!(frame_chunk(1, b"hi", true, false, 0), b"hi".to_vec());
    }

    #[test]
    fn frame_chunk_nontty_no_timestamps_uses_log_frame() {
        // Non-TTY without timestamps is exactly one multiplexed frame.
        assert_eq!(frame_chunk(1, b"hi", false, false, 0), log_frame(1, b"hi"));
    }

    #[test]
    fn frame_chunk_tty_timestamps_prefixes_each_line() {
        // With timestamps + TTY, each split line gets an RFC3339 prefix and a space, no demux header.
        let ts = fmt_rfc3339(1_700_000_000); // "2023-11-14T22:13:20Z"
        let out = frame_chunk(1, b"a\nb", true, true, 1_700_000_000);
        let expected = format!("{ts} a\n{ts} b");
        assert_eq!(out, expected.into_bytes());
    }

    #[test]
    fn frame_chunk_nontty_timestamps_frames_each_stamped_line() {
        // With timestamps + non-TTY, each stamped line is wrapped in its own log frame.
        let ts = fmt_rfc3339(1_700_000_000);
        let out = frame_chunk(2, b"a\n", false, true, 1_700_000_000);
        let expected = log_frame(2, format!("{ts} a\n").as_bytes());
        assert_eq!(out, expected);
    }

    #[test]
    fn persisted_ordered_stdout_then_stderr() {
        // stdout is stamped with start_t, stderr with end_t; empties are omitted.
        let v = persisted_ordered(b"out".to_vec(), b"err".to_vec(), 100, 200);
        assert_eq!(v, vec![(100, 1u8, b"out".to_vec()), (200, 2u8, b"err".to_vec())]);
    }

    #[test]
    fn persisted_ordered_omits_empty_streams() {
        assert_eq!(
            persisted_ordered(b"out".to_vec(), Vec::new(), 100, 200),
            vec![(100, 1u8, b"out".to_vec())]
        );
        assert!(persisted_ordered(Vec::new(), Vec::new(), 1, 2).is_empty());
    }
}

pub(crate) async fn containers_logs(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<LogsQ>,
) -> Response {
    let follow = q_truthy(&q.follow);
    let timestamps = q_truthy(&q.timestamps);
    // Stream selection: honor explicit stdout/stderr flags, defaulting to both when neither is given.
    let (mut want_out, mut want_err) = (q_truthy(&q.stdout), q_truthy(&q.stderr));
    if !want_out && !want_err {
        want_out = true;
        want_err = true;
    }
    // `--tail`: "all"/absent/unparsable -> everything; a number -> that many trailing lines.
    let tail = match q.tail.as_deref() {
        None | Some("") | Some("all") => None,
        Some(s) => s.parse::<usize>().ok(),
    };
    let since = parse_unix_ts(&q.since);
    let until = parse_unix_ts(&q.until);

    // Snapshot the container + its live IO under the daemon lock; clone the Arc<Live> so we can read its
    // buffers / subscribe to its broadcast after releasing the lock.
    let (tty, running, live, persisted_out, persisted_err, start_t, end_t) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &id) else {
            return no_such(&id);
        };
        let Some(c) = g.containers.get(&full) else {
            return no_such(&id);
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

    // For follow we stream new output from the container's `Live.out` broadcast (the same channel
    // attach/exec fan out from). Subscribe BEFORE snapshotting the buffer so output produced between the
    // snapshot and the stream start isn't lost -- a chunk straddling that boundary may appear once in
    // the replay and once live, i.e. dd favors never dropping output over a rare duplicate.
    let follow_live = follow && running && live.is_some();
    let out_sub = if follow_live {
        live.as_ref().map(|l| l.out.subscribe())
    } else {
        None
    };
    // Follow ends on `out_done` (pumps fully drained), NOT the immediate `exit`, so a fast-exiting
    // guest's final lines aren't lost to the pump race (mirrors the attach/exec hijack writer).
    let exit_sub = live.as_ref().map(|l| l.out_done_rx.clone());

    // Buffered output, as the single chronological record `(emit-secs, stream, bytes)`. While the Live
    // exists (running, or exited-but-retained for non-`--rm`) we read its ordered `log_chunks` directly,
    // so stdout/stderr replay interleaved. Once the ordered log is gone (e.g. a daemon restart, where the
    // per-stream persisted buffers are also empty since they aren't serialized) we fall back to an
    // approximate stdout-then-stderr ordering from `cc.stdout`/`cc.stderr`, the best possible then.
    let chunks: Vec<(i64, u8, Vec<u8>)> = match &live {
        Some(l) => {
            let lc = l.log_chunks.lock().await;
            if !lc.is_empty() {
                lc.clone()
            } else {
                drop(lc);
                persisted_ordered(persisted_out, persisted_err, start_t, end_t)
            }
        }
        None => persisted_ordered(persisted_out, persisted_err, start_t, end_t),
    };

    // Replay: walk the ordered log, coalescing adjacent same-stream chunks into runs so line-splitting
    // sees contiguous stream output (clean lines) while stream switches stay interleave points. Each run
    // carries its first chunk's emit time, which drives `--since`/`--until` (per-line, by recorded time)
    // and `--timestamps`. Then `--tail` keeps the last N lines of the combined ordered set, and each line
    // is per-line timestamped + framed (multiplexed 8-byte header for non-TTY, raw bytes for TTY).
    let mut runs: Vec<(i64, u8, Vec<u8>)> = Vec::new();
    for (ts, stream, data) in &chunks {
        match runs.last_mut() {
            Some((_, s, buf)) if *s == *stream => buf.extend_from_slice(data),
            _ => runs.push((*ts, *stream, data.clone())),
        }
    }
    let mut entries: Vec<(i64, u8, Vec<u8>)> = Vec::new();
    for (ts, stream, data) in &runs {
        if (*stream == 1 && !want_out) || (*stream == 2 && !want_err) {
            continue;
        }
        if since.map_or(false, |s| *ts < s) || until.map_or(false, |u| *ts > u) {
            continue;
        }
        for line in split_log_lines(data) {
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

    // Follow: a task emits the replay, then streams each new broadcast chunk until the container exits
    // (draining any final buffered chunks first) or the client disconnects (the channel send fails).
    // `until`, when given, also ends the stream once wall-clock passes it.
    let mut out_rx = out_sub.unwrap();
    let mut done_rx = exit_sub.unwrap();
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
        if !replay.is_empty() && tx.send(replay).await.is_err() {
            return;
        }
        let want = |kind: u8| (kind == 1 && want_out) || (kind == 2 && want_err);
        // If the guest finished AND drained between the snapshot and here, out_done already holds true.
        let mut exited = *done_rx.borrow();
        loop {
            if exited {
                // The broadcast Sender stays alive in the live map, so recv() would block forever after
                // exit; flush whatever is still buffered with try_recv, then end the stream.
                while let Ok((kind, chunk)) = out_rx.try_recv() {
                    if want(kind) {
                        let f = frame_chunk(kind, &chunk, tty, timestamps, now_secs());
                        if tx.send(f).await.is_err() {
                            return;
                        }
                    }
                }
                break;
            }
            if let Some(u) = until {
                if now_secs() > u {
                    break;
                }
            }
            tokio::select! {
                biased;
                msg = out_rx.recv() => match msg {
                    Ok((kind, chunk)) => {
                        if want(kind) {
                            let f = frame_chunk(kind, &chunk, tty, timestamps, now_secs());
                            if tx.send(f).await.is_err() { return; }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = done_rx.changed() => { exited = true; }
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
