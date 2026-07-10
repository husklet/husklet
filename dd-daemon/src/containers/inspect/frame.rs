//! Pure log-framing helpers for `docker logs`: timestamp parsing, line splitting, wire framing, and
//! the persisted-buffer fallback ordering. No async / no I/O — see `logs.rs` for the handler.
use super::super::*;

/// Parse a docker `since`/`until` value into unix seconds. Docker accepts unix `secs[.nanos]`, RFC3339 /
/// RFC3339Nano, and Go durations relative to now — all handled via the shared [`crate::util::parse_docker_ts`].
/// Returns None for absent/0/unparsable (a 0/negative result disables the filter, matching prior behavior).
pub(super) fn parse_unix_ts(s: &Option<String>) -> Option<i64> {
    let v = s.as_deref().filter(|x| !x.is_empty())?;
    crate::util::parse_docker_ts(v, crate::util::now_secs()).filter(|n| *n > 0)
}

/// Split a log buffer into newline-terminated lines, keeping the trailing `\n` on each line and any
/// final unterminated fragment as its own line. Used to apply `--tail` and `--timestamps` per line.
pub(super) fn split_log_lines(buf: &[u8]) -> Vec<Vec<u8>> {
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
pub(super) fn frame_chunk(stream: u8, data: &[u8], tty: bool, timestamps: bool, ts_secs: i64) -> Vec<u8> {
    if !timestamps {
        return if tty {
            data.to_vec()
        } else {
            log_frame(stream, data)
        };
    }
    // Docker `logs --timestamps` emits RFC3339Nano (zero-padded fractional nanoseconds), e.g.
    // `2023-11-14T22:13:20.000000000Z`. Our chunk times are second-granular, so pad the fraction to nine
    // zeros rather than dropping it — matching docker's wire shape (a second-precision stamp diverged).
    let ts = fmt_rfc3339_nanos(ts_secs.saturating_mul(1_000_000_000));
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
pub(super) fn persisted_ordered(
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
    fn parse_unix_ts_accepts_docker_rfc3339_forms() {
        // Docker also sends RFC3339 / RFC3339Nano for --since/--until; these must apply the filter,
        // not disable it (a previous integer-only parse returned None and streamed everything).
        assert_eq!(parse_unix_ts(&Some("2023-11-14T22:13:20Z".into())), Some(1_700_000_000));
        assert_eq!(
            parse_unix_ts(&Some("2023-11-14T22:13:20.123456789Z".into())),
            Some(1_700_000_000)
        );
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
        // With timestamps + TTY, each split line gets an RFC3339Nano prefix and a space, no demux header.
        let ts = fmt_rfc3339_nanos(1_700_000_000i64 * 1_000_000_000); // "2023-11-14T22:13:20.000000000Z"
        let out = frame_chunk(1, b"a\nb", true, true, 1_700_000_000);
        let expected = format!("{ts} a\n{ts} b");
        assert_eq!(out, expected.into_bytes());
    }

    #[test]
    fn frame_chunk_timestamps_use_rfc3339nano_shape() {
        // Docker `logs --timestamps` pads the fraction to nine zeros, not a bare second stamp.
        let out = frame_chunk(1, b"a\n", true, true, 1_700_000_000);
        assert_eq!(out, b"2023-11-14T22:13:20.000000000Z a\n".to_vec());
    }

    #[test]
    fn frame_chunk_nontty_timestamps_frames_each_stamped_line() {
        // With timestamps + non-TTY, each stamped line is wrapped in its own log frame.
        let ts = fmt_rfc3339_nanos(1_700_000_000i64 * 1_000_000_000);
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
