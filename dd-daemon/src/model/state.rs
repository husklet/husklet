#![allow(unused_imports, dead_code)]
use super::*;

/// A running container's live IO plumbing. Created on first attach-or-start, dropped when the guest
/// process exits. The process stdout/stderr fan out to (a) any attached clients via `out`, (b) the log
/// buffers for `docker logs`. `stdin` feeds the guest for `-i`/attach.
pub(crate) struct Live {
    pub(crate) out: broadcast::Sender<(u8, Vec<u8>)>, // (1=stdout, 2=stderr, chunk)
    pub(crate) stdin_tx: mpsc::Sender<Vec<u8>>, // attach writes here; an empty Vec = stdin EOF
    pub(crate) stdin_rx: Mutex<Option<mpsc::Receiver<Vec<u8>>>>, // start() takes it and feeds the guest
    /// Chronological `docker logs` replay record: one entry per output chunk, in arrival order, as
    /// `(emit unix-secs, stream 1=stdout/2=stderr, bytes)`. A single ordered log (replacing the old
    /// per-stream `stdout_buf`/`stderr_buf`) so the buffered replay interleaves stdout/stderr exactly as
    /// the live `out` broadcast does. The reaper derives the per-stream `cc.stdout`/`cc.stderr` from it.
    pub(crate) log_chunks: Arc<Mutex<Vec<(i64, u8, Vec<u8>)>>>,
    pub(crate) exit: watch::Sender<Option<i64>>, // Some(code) once exited
    pub(crate) exit_rx: watch::Receiver<Option<i64>>,
    /// Fired `true` once NO more output will ever reach `out`: the reaper sets it only AFTER the
    /// stdout/stderr pump tasks have fully drained the guest's pipes/PTY into the broadcast. `exit`
    /// fires the instant the process dies (so `wait`/inspect/logs stay responsive), but at that moment
    /// the pumps -- separate tasks -- may not have broadcast the final bytes yet. A streaming consumer
    /// (attach/exec hijack, `logs -f`) that closed on `exit` alone would race the pumps and drop a
    /// fast-exiting command's last output; closing on `out_done` instead guarantees a complete stream.
    pub(crate) out_done: watch::Sender<bool>,
    pub(crate) out_done_rx: watch::Receiver<bool>,
    pub(crate) started: std::sync::atomic::AtomicBool, // start() spawns the process exactly once
    pub(crate) stop_requested: std::sync::atomic::AtomicBool, // set by stop/kill/rm so the RestartPolicy supervisor won't auto-restart a deliberately-stopped container
    pub(crate) tty: bool,
    pub(crate) pty_master: std::sync::Mutex<Option<RawFd>>, // the PTY master fd (tty containers) for /resize
    pub(crate) pid: std::sync::Mutex<Option<u32>>, // the live JIT process pid (for pause = SIGSTOP/SIGCONT)
}

impl Live {
    pub(crate) fn new(tty: bool) -> Arc<Self> {
        let (out, _) = broadcast::channel(1024);
        let (exit, exit_rx) = watch::channel(None);
        let (out_done, out_done_rx) = watch::channel(false);
        let (stdin_tx, stdin_rx) = mpsc::channel(256);
        Arc::new(Live {
            out,
            stdin_tx,
            stdin_rx: Mutex::new(Some(stdin_rx)),
            log_chunks: Arc::new(Mutex::new(Vec::new())),
            exit,
            exit_rx,
            out_done,
            out_done_rx,
            started: std::sync::atomic::AtomicBool::new(false),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            tty,
            pty_master: std::sync::Mutex::new(None),
            pid: std::sync::Mutex::new(None),
        })
    }
}

#[derive(Default)]
pub(crate) struct Inner {
    pub(crate) containers: HashMap<String, Container>,
    pub(crate) images: Vec<Image>,
    pub(crate) volumes: Vec<Vol>,
    pub(crate) networks: Vec<Net>,
    pub(crate) live: HashMap<String, Arc<Live>>, // running containers' (and execs') IO plumbing (not persisted)
    pub(crate) execs: HashMap<String, Exec>,     // exec id -> its spec
}

#[derive(Clone)]
pub(crate) struct App {
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) state_path: String,
    pub(crate) volumes_dir: String,
    pub(crate) images_dir: String,
    pub(crate) events: crate::events::EventBus, // docker events lifecycle bus
}
