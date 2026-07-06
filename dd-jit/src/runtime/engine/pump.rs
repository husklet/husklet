use super::*;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::io::unix::AsyncFd;

/// Reap `pid` on a blocking thread, decoding `waitpid` status into an exit code (`128+signum` when
/// signalled, per the Docker/shell convention).
pub(crate) async fn reap(pid: u32) -> i64 {
    tokio::task::spawn_blocking(move || {
        let mut status: i32 = 0;
        // SAFETY: waitpid on our own forked child's pid with a valid status out-pointer.
        let r = unsafe { libc::waitpid(pid as i32, &mut status, 0) };
        if r < 0 {
            -1
        } else {
            crate::runtime::handle::decode_wait_status(status) as i64
        }
    })
    .await
    .unwrap_or(-1)
}

/// Append one chunk to the rotated replay buffer, enforcing [`LOG_CHUNKS_CAP_BYTES`] by draining the
/// oldest chunks (but always keeping the just-pushed one).
async fn push_log(log_chunks: &Arc<tokio::sync::Mutex<Vec<LogChunk>>>, ts: i64, stream: u8, bytes: Vec<u8>) {
    let mut log = log_chunks.lock().await;
    log.push((ts, stream, bytes));
    let mut total: usize = log.iter().map(|(_, _, b)| b.len()).sum();
    let mut drop_to = 0;
    while total > LOG_CHUNKS_CAP_BYTES && drop_to < log.len() - 1 {
        total -= log[drop_to].2.len();
        drop_to += 1;
    }
    if drop_to > 0 {
        log.drain(..drop_to);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pump a readable fd (stdout/stderr pipe read end, or the PTY master) into the broadcast + rotated log
/// under `kind` (1=stdout, 2=stderr; a PTY merges to 1). Ends on EOF / EIO when the guest exits.
pub(crate) async fn pump_fd(
    afd: Arc<AsyncFd<OwnedFd>>,
    kind: u8,
    out: broadcast::Sender<(u8, Vec<u8>)>,
    log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>>,
) {
    loop {
        let Ok(mut g) = afd.readable().await else { break };
        let mut buf = [0u8; 8192];
        match g.try_io(|i| read_fd(i.as_raw_fd(), &mut buf)) {
            Ok(Ok(0)) | Ok(Err(_)) => break, // EOF / EIO when the guest exits
            Ok(Ok(n)) => {
                let chunk = buf[..n].to_vec();
                let _ = out.send((kind, chunk.clone()));
                push_log(&log_chunks, now_secs(), kind, chunk).await;
            }
            Err(_would_block) => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{push_log, LogChunk, LOG_CHUNKS_CAP_BYTES};
    use std::sync::Arc;

    fn total_bytes(log: &[LogChunk]) -> usize {
        log.iter().map(|(_, _, b)| b.len()).sum()
    }

    #[tokio::test]
    async fn rotation_drops_oldest_over_cap() {
        let log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        // Push chunks whose sum exceeds the cap; each is a comfortable fraction of it so several fit.
        let chunk = LOG_CHUNKS_CAP_BYTES / 4;
        for i in 0..10i64 {
            push_log(&log_chunks, i, 1, vec![i as u8; chunk]).await;
        }
        let log = log_chunks.lock().await;
        // The oldest chunks were drained so the retained total stays within the cap...
        assert!(total_bytes(&log) <= LOG_CHUNKS_CAP_BYTES, "total {} exceeds cap", total_bytes(&log));
        // ...but the most-recent chunk is always kept, and rotation trims from the front (FIFO).
        assert_eq!(log.last().unwrap().0, 9);
        assert!(log.len() < 10, "expected oldest chunks to be dropped");
        // The retained window is contiguous and ends at the newest chunk (oldest-first order preserved).
        for w in log.windows(2) {
            assert_eq!(w[0].0 + 1, w[1].0);
        }
    }

    #[tokio::test]
    async fn single_oversized_chunk_is_retained() {
        let log_chunks: Arc<tokio::sync::Mutex<Vec<LogChunk>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        // A lone chunk larger than the whole cap: the `drop_to < len-1` guard always keeps the
        // just-pushed chunk, so it survives even though it alone blows the cap.
        let big = vec![0u8; LOG_CHUNKS_CAP_BYTES + 4096];
        push_log(&log_chunks, 0, 1, big).await;
        let log = log_chunks.lock().await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].2.len(), LOG_CHUNKS_CAP_BYTES + 4096);

        // And a subsequent normal chunk drops the oversized one (it's now the oldest, and no longer the
        // just-pushed chunk), leaving only the newest.
        drop(log);
        push_log(&log_chunks, 1, 1, vec![7u8; 1024]).await;
        let log = log_chunks.lock().await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, 1);
    }
}
