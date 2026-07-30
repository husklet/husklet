use hl_gpu::{
    Capabilities, Cmd, CommandBuffer, CommandSink, FenceId, GpuError, ReadbackRequest,
    RemoteCommandSink, TransportConfig, TransportError, TransportPhase,
};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

static SOCKET_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct Socket(PathBuf);

impl Socket {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "hl-gpu-{name}-{}-{}.sock",
            std::process::id(),
            SOCKET_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn config() -> TransportConfig {
    TransportConfig::new(
        Duration::from_millis(200),
        Duration::from_millis(80),
        Duration::from_millis(80),
        Duration::from_millis(80),
    )
    .unwrap()
}

fn handshake(stream: &UnixStream) {
    hl_gpu::transport::adapter::unix::Connection::new(stream)
        .write_handshake(&Capabilities::full("test"))
        .unwrap();
}

fn frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = [0; 16];
    stream.read_exact(&mut header).unwrap();
    let len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    let mut body = vec![0; len];
    stream.read_exact(&mut body).unwrap();
    body
}

fn readback(stream: &mut UnixStream) -> ReadbackRequest {
    ReadbackRequest::from_bytes(&frame(stream)).unwrap()
}

#[test]
fn failure_before_request_retries_without_duplicate_execution() {
    let socket = Socket::new("prewrite");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (first, _) = listener.accept().unwrap();
        drop(first);
        let (mut second, _) = listener.accept().unwrap();
        handshake(&second);
        let body = frame(&mut second);
        second.write_all(&[hl_gpu::transport::ACK_OK]).unwrap();
        hl_gpu::Decoder::stream(&body).unwrap()
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    sink.submit(&[Cmd::CreateFence(7)]).unwrap();
    assert_eq!(server.join().unwrap(), vec![Cmd::CreateFence(7)]);
    assert_eq!(sink.connects(), 1);
}

#[test]
fn applied_frame_without_ack_is_terminal_and_never_replayed() {
    let socket = Socket::new("no-ack");
    let listener = UnixListener::bind(&socket.0).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let first = loop {
            match listener.accept() {
                Ok(pair) => break pair.0,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::yield_now();
                }
                Err(error) => panic!("{error}"),
            }
        };
        let mut first = first;
        handshake(&first);
        let body = frame(&mut first);
        thread::sleep(Duration::from_millis(160));
        let reconnected = listener.accept().is_ok();
        (hl_gpu::Decoder::stream(&body).unwrap(), reconnected)
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    let error = sink.submit(&[Cmd::CreateFence(11)]).unwrap_err();
    assert!(matches!(
        error,
        GpuError::Transport(TransportError::Timeout {
            phase: TransportPhase::Acknowledgement,
            ambiguous: true
        })
    ));
    assert!(matches!(
        sink.submit(&[Cmd::CreateFence(12)]),
        Err(GpuError::Transport(TransportError::Poisoned { .. }))
    ));
    let (executed, reconnected) = server.join().unwrap();
    assert_eq!(executed, vec![Cmd::CreateFence(11)]);
    assert!(!reconnected);
}

#[test]
fn handshake_stall_is_bounded_and_safe_to_retry() {
    let socket = Socket::new("handshake");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (_first, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(120));
        let (_second, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(120));
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    let started = Instant::now();
    let error = sink.submit(&[Cmd::CreateFence(1)]).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        error,
        GpuError::Transport(TransportError::Timeout {
            phase: TransportPhase::Handshake,
            ambiguous: false
        })
    ));
    assert!(sink.terminal_error().is_none());
    server.join().unwrap();
}

#[test]
fn partial_frame_write_timeout_is_terminal() {
    use hl_gpu::protocol::model::descriptor::BufferDesc;
    use hl_gpu::protocol::model::enums::buffer_usage;

    let socket = Socket::new("partial");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handshake(&stream);
        thread::sleep(Duration::from_millis(240));
    });
    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    let commands = [
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 32 << 20,
                usage: buffer_usage::COPY_DST,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![1; 32 << 20],
        },
    ];
    let error = sink.submit(&commands).unwrap_err();
    assert!(matches!(
        error,
        GpuError::Transport(TransportError::Timeout {
            phase: TransportPhase::FrameWrite,
            ambiguous: true
        })
    ));
    assert!(sink.terminal_error().is_some());
    server.join().unwrap();
}

#[test]
fn invalid_fence_response_poisons_following_requests() {
    use hl_gpu::transport::model::readback::READBACK_OK;

    let socket = Socket::new("bad-fence");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        let _ = frame(&mut stream);
        hl_gpu::transport::adapter::unix::Connection::new(&stream)
            .write_readback_response(READBACK_OK, &[2])
            .unwrap();
    });
    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    assert!(matches!(
        sink.poll_fence(hl_gpu::FenceId(1), 1),
        Err(GpuError::Transport(TransportError::Ambiguous {
            phase: TransportPhase::ResponseRead,
            ..
        }))
    ));
    assert!(matches!(
        sink.poll_fence(hl_gpu::FenceId(1), 1),
        Err(GpuError::Transport(TransportError::Poisoned { .. }))
    ));
    server.join().unwrap();
}

#[test]
fn reconnect_restores_fence_residency_before_polling() {
    let socket = Socket::new("fence-replay");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        handshake(&first);
        let original = frame(&mut first);
        first.write_all(&[hl_gpu::transport::ACK_OK]).unwrap();
        drop(first);
        closed_tx.send(()).unwrap();

        let (mut second, _) = listener.accept().unwrap();
        handshake(&second);
        let replay = frame(&mut second);
        second.write_all(&[hl_gpu::transport::ACK_OK]).unwrap();
        let request = readback(&mut second);
        assert_eq!(request.id, 7);
        hl_gpu::transport::adapter::unix::Connection::new(&second)
            .write_readback_response(hl_gpu::transport::model::readback::READBACK_OK, &[1])
            .unwrap();
        (
            hl_gpu::Decoder::stream(&original).unwrap(),
            hl_gpu::Decoder::stream(&replay).unwrap(),
        )
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    let program = [
        Cmd::CreateFence(7),
        Cmd::Submit(CommandBuffer {
            encoder: Vec::new(),
            signal: Some((7, 3)),
        }),
    ];
    sink.submit(&program).unwrap();
    closed_rx.recv().unwrap();
    assert!(sink.poll_fence(FenceId(7), 3).unwrap());
    let (original, replay) = server.join().unwrap();
    assert_eq!(original, program);
    assert_eq!(replay, program);
}

#[test]
fn finite_fence_wait_can_exceed_base_response_deadline() {
    let socket = Socket::new("finite-wait");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        let request = readback(&mut stream);
        assert_eq!(request.len, 180_000_000);
        thread::sleep(Duration::from_millis(130));
        hl_gpu::transport::adapter::unix::Connection::new(&stream)
            .write_readback_response(hl_gpu::transport::model::readback::READBACK_OK, &[1])
            .unwrap();
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    assert_eq!(
        sink.wait_timeout(FenceId(1), 1, 180_000_000).unwrap(),
        hl_gpu::FenceWait::Complete
    );
    server.join().unwrap();
}

#[test]
fn unbounded_fence_wait_uses_bounded_requests_until_complete() {
    let socket = Socket::new("unbounded-wait");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        for status in [0, 1] {
            let request = readback(&mut stream);
            assert_eq!(request.len, 1_000_000_000);
            hl_gpu::transport::adapter::unix::Connection::new(&stream)
                .write_readback_response(hl_gpu::transport::model::readback::READBACK_OK, &[status])
                .unwrap();
        }
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    sink.wait(FenceId(1), 1).unwrap();
    server.join().unwrap();
}

#[test]
fn explicit_readback_rejection_preserves_the_connection() {
    let socket = Socket::new("rejection");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        let _ = readback(&mut stream);
        hl_gpu::transport::adapter::unix::Connection::new(&stream)
            .write_readback_response(hl_gpu::transport::model::readback::READBACK_FAIL, &[])
            .unwrap();
        let submitted = frame(&mut stream);
        stream.write_all(&[hl_gpu::transport::ACK_OK]).unwrap();
        hl_gpu::Decoder::stream(&submitted).unwrap()
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    assert!(matches!(
        sink.poll_fence(FenceId(99), 1),
        Err(GpuError::Transport(TransportError::Rejected {
            phase: TransportPhase::ResponseRead,
            ..
        }))
    ));
    assert!(sink.terminal_error().is_none());
    sink.submit(&[Cmd::CreateFence(4)]).unwrap();
    assert_eq!(server.join().unwrap(), vec![Cmd::CreateFence(4)]);
}

#[test]
fn eof_after_applied_frame_is_non_timeout_ambiguous_and_terminal() {
    let socket = Socket::new("eof-ack");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        let submitted = frame(&mut stream);
        drop(stream);
        hl_gpu::Decoder::stream(&submitted).unwrap()
    });

    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    assert!(matches!(
        sink.submit(&[Cmd::CreateFence(5)]),
        Err(GpuError::Transport(TransportError::Ambiguous {
            phase: TransportPhase::Acknowledgement,
            ..
        }))
    ));
    assert!(matches!(
        sink.submit(&[Cmd::CreateFence(6)]),
        Err(GpuError::Transport(TransportError::Poisoned { .. }))
    ));
    assert_eq!(server.join().unwrap(), vec![Cmd::CreateFence(5)]);
}

#[test]
fn partial_response_header_timeout_is_terminal() {
    let socket = Socket::new("partial-header");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        let _ = readback(&mut stream);
        stream
            .write_all(&[hl_gpu::transport::model::readback::READBACK_OK])
            .unwrap();
        thread::sleep(Duration::from_millis(180));
    });
    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    assert!(matches!(
        sink.poll_fence(FenceId(1), 1),
        Err(GpuError::Transport(TransportError::Timeout {
            phase: TransportPhase::ResponseRead,
            ambiguous: true
        }))
    ));
    server.join().unwrap();
}

#[test]
fn partial_response_body_timeout_is_terminal() {
    let socket = Socket::new("partial-body");
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handshake(&stream);
        let _ = readback(&mut stream);
        stream
            .write_all(&[hl_gpu::transport::model::readback::READBACK_OK, 1, 0, 0, 0])
            .unwrap();
        thread::sleep(Duration::from_millis(180));
    });
    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    assert!(matches!(
        sink.poll_fence(FenceId(1), 1),
        Err(GpuError::Transport(TransportError::Timeout {
            phase: TransportPhase::ResponseRead,
            ambiguous: true
        }))
    ));
    server.join().unwrap();
}

#[test]
fn failed_connect_is_bounded_and_typed_before_request() {
    let socket = Socket::new("connect");
    let mut sink = RemoteCommandSink::with_config(socket.0.to_string_lossy(), config());
    let started = Instant::now();
    assert!(matches!(
        sink.submit(&[Cmd::CreateFence(1)]),
        Err(GpuError::Transport(TransportError::Unavailable {
            phase: TransportPhase::Connect,
            ..
        }))
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(sink.terminal_error().is_none());
}

#[test]
fn connect_deadline_bounds_a_saturated_unix_backlog() {
    let socket = Socket::new("connect-backlog");
    let listener =
        socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
    listener
        .bind(&socket2::SockAddr::unix(&socket.0).unwrap())
        .unwrap();
    listener.listen(0).unwrap();

    let mut queued = Vec::new();
    let started = Instant::now();
    let timeout = loop {
        match hl_gpu::transport::adapter::unix::Connection::connect(
            &socket.0,
            Duration::from_millis(40),
        ) {
            Ok(stream) if queued.len() < 128 => queued.push(stream),
            Ok(_) => panic!("Unix listener backlog did not saturate"),
            Err(error) => break error,
        }
    };
    assert!(
        matches!(
            timeout.kind(),
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::Other
        ),
        "{timeout:?}"
    );
    assert!(
        !queued.is_empty(),
        "the backlog was populated before refusal"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}
