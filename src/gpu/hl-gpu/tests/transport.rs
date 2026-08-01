//! `transport` integration tests over a real `UnixListener`: a [`RemoteCommandSink`] submits batches; a
//! [`serve_connection`] server on the other end decodes them. We assert the decoded batch equals what was
//! submitted (bytes + structure), that the socketed path agrees with protocol's in-memory [`RecordingSink`]
//! at the command boundary, and that the ack (NACK) + reconnect/residency-replay paths behave.

use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use hl_gpu::protocol::model::capability::{shader_payload, ALL_COMMANDS, COLOR_FORMATS};
use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::transport::model::header::ACK_OK;
use hl_gpu::{
    serve_connection, Capabilities, Cmd, CommandSink, FeatureRequest, RecordingSink,
    RemoteCommandSink, Surface, WIRE_VERSION,
};

/// A unique temp socket path for one test, cleaned up by [`TempSock`] on drop.
struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-transport-{tag}-{}-{:?}.sock",
            std::process::id(),
            thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        TempSock(p)
    }
    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
impl Drop for TempSock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn sample_batch() -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 64,
                usage: buffer_usage::STORAGE,
                label: "b".into(),
            },
        ),
        Cmd::CreateFence(2),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::Dispatch { x: 4, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: Some((2, 5)),
        }),
        Cmd::Present {
            surface: 9,
            texture: 3,
            serial: hl_gpu::FrameSerial::new(4).unwrap(),
        },
    ]
}

fn feature_request() -> FeatureRequest {
    FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::SPIRV,
        command_bits: hl_gpu::Capabilities::command_bits(ALL_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    }
}

#[test]
fn remote_sink_submits_a_batch_a_server_decodes_identically() {
    let sock = TempSock::new("roundtrip");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let batch = sample_batch();
    let expected_payload = hl_gpu::Encoder::stream(&batch);

    // Server: advertise caps, decode exactly one frame, report it back to the test.
    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::permissive_fixture("host");
        serve_connection(&stream, &caps, |header, decoded: &[Cmd]| {
            tx.send((*header, decoded.to_vec())).unwrap();
            true // ACK_OK
        })
        .unwrap();
    });

    let surface = Surface {
        id: 9,
        width: 640,
        height: 480,
        stride: 2560,
        fd: -1,
        generation: 0,
    };
    let mut sink = RemoteCommandSink::with_surface(sock.path(), surface);
    sink.submit(&batch).expect("submit acknowledged");
    drop(sink); // close the connection so the server loop returns

    let (header, decoded) = rx.recv().unwrap();
    server.join().unwrap();

    // Structure: the decoded batch equals what was submitted.
    assert_eq!(
        decoded, batch,
        "server decoded the exact submitted command sequence"
    );
    // Bytes: the payload the server saw re-encodes to the same bytes the sink encoded.
    assert_eq!(
        hl_gpu::Encoder::stream(&decoded),
        expected_payload,
        "payload bytes are byte-identical"
    );
    // The transport-private 16-byte header carried the surface id/geometry.
    assert_eq!(
        (header.surface_id, header.width, header.height),
        (9, 640, 480)
    );
    assert_eq!(header.len as usize, expected_payload.len());
}

#[test]
fn remote_sink_and_recording_sink_agree_at_the_command_boundary() {
    // Prove equivalence: the same batch through RemoteCommandSink (socket) and through the in-memory
    // RecordingSink yields the same decoded command sequence at the boundary.
    let sock = TempSock::new("equiv");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let batch = sample_batch();

    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::permissive_fixture("host");
        serve_connection(&stream, &caps, move |_h, decoded: &[Cmd]| {
            tx.send(decoded.to_vec()).unwrap();
            true
        })
        .unwrap();
    });

    // In-memory reference sink.
    let mut recording = RecordingSink::with_full_caps();
    recording.submit(&batch).unwrap();

    // Socketed sink.
    let mut remote = RemoteCommandSink::new(sock.path());
    remote.submit(&batch).unwrap();
    drop(remote);

    let socketed = rx.recv().unwrap();
    server.join().unwrap();

    let recorded: Vec<Cmd> = recording.commands().cloned().collect();
    assert_eq!(
        socketed, recorded,
        "socketed boundary == in-memory boundary"
    );
    assert_eq!(socketed, batch);
}

#[test]
fn remote_sink_negotiates_against_the_host_handshake() {
    // The server advertises a capability handshake on connect; negotiate() reads it and checks the guest's
    // required features against it before any command flows.
    let sock = TempSock::new("negotiate");
    let listener = UnixListener::bind(&sock.0).unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::permissive_fixture("host");
        // Serve until the client disconnects (it only negotiates + drops here).
        serve_connection(&stream, &caps, |_h, _b: &[Cmd]| true).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    let caps = sink
        .negotiate(&feature_request())
        .expect("compatible negotiation");
    assert_eq!(caps.wire_version, WIRE_VERSION);
    assert!(caps.supports_shader_payload(shader_payload::SPIRV));

    // Incompatible: an MSL-only guest against a host that omits MSL... instead use a wire-version bump,
    // which the full host cannot satisfy — a clean typed error, not a runtime BadTag.
    let bad = FeatureRequest {
        wire_version: WIRE_VERSION + 1,
        ..feature_request()
    };
    assert!(
        sink.negotiate(&bad).is_err(),
        "incompatible request fails cleanly at negotiation"
    );

    drop(sink);
    server.join().unwrap();
}

#[test]
fn a_failure_ack_makes_submit_return_err() {
    // A host that reads the frame and replies ACK_FAIL must make submit() return Err — never treated as a
    // successful present. The NACK is NOT retried (the connection is healthy; the host reported failure).
    let sock = TempSock::new("nack");
    let listener = UnixListener::bind(&sock.0).unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::permissive_fixture("host");
        // Reject exactly one frame, then the client should give up (no retry) and disconnect.
        serve_connection(&stream, &caps, |_h, _b: &[Cmd]| false /* NACK */).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    let result = sink.submit(&[Cmd::CreateFence(1)]);
    assert!(
        result.is_err(),
        "RemoteCommandSink treated a NACK as success"
    );
    assert_eq!(
        sink.connects(),
        1,
        "a NACK is not retried on a fresh connection"
    );
    drop(sink);
    server.join().unwrap();
}

#[test]
fn reconnect_replays_acknowledged_residency_once_before_new_work() {
    // Kill a live executor after it acks the upload, before the dependent draw; the reconnected executor
    // must receive the complete residency replayed ahead of the new work, exactly once.
    let sock = TempSock::new("reconnect");
    let listener = UnixListener::bind(&sock.0).unwrap();

    let upload = vec![
        Cmd::CreateBuffer(
            4,
            BufferDesc {
                size: 4,
                usage: buffer_usage::COPY_DST,
                label: "resident".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 4,
            offset: 0,
            data: vec![1, 2, 3, 4],
        },
    ];
    let draw = vec![
        Cmd::Submit(CommandBuffer::default()),
        Cmd::Present {
            surface: 9,
            texture: 8,
            serial: hl_gpu::FrameSerial::new(10).unwrap(),
        },
    ];

    let caps_bytes = Capabilities::permissive_fixture("host");
    // The client may only start the second submit once connection #1 is actually closed: otherwise it
    // writes the draw into a still-open socket and reads EOF at the ack, which is (correctly) ambiguous
    // and terminal. Ordering by an explicit close event keeps the test independent of thread scheduling.
    let (closed_tx, closed_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        // Read one framed submit's payload after sending the handshake.
        fn hs_then_frame(stream: &mut UnixStream, caps: &Capabilities) -> Vec<u8> {
            hl_gpu::transport::adapter::unix::Connection::new(stream)
                .write_handshake(caps)
                .unwrap();
            let mut hdr = [0u8; 16];
            stream.read_exact(&mut hdr).unwrap();
            let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).unwrap();
            body
        }
        use std::io::Write;

        // Connection #1: accept the upload, ACK, then die before the draw.
        let (mut first, _) = listener.accept().unwrap();
        let first_body = hs_then_frame(&mut first, &caps_bytes);
        first.write_all(&[ACK_OK]).unwrap();
        drop(first);
        closed_tx.send(()).unwrap();

        // Connection #2: a fresh executor. It must get residency replayed ahead of the new work.
        let (mut second, _) = listener.accept().unwrap();
        let recovered = hs_then_frame(&mut second, &caps_bytes);
        second.write_all(&[ACK_OK]).unwrap();
        (first_body, recovered)
    });

    let surface = Surface {
        id: 9,
        width: 8,
        height: 8,
        stride: 32,
        fd: -1,
        generation: 0,
    };
    let mut sink = RemoteCommandSink::with_surface(sock.path(), surface);
    sink.submit(&upload).expect("residency frame acknowledged");
    closed_rx.recv().expect("first executor closed its socket");
    sink.submit(&draw)
        .expect("reconnect recovered residency and executor ACKed");

    let (first_body, recovered) = server.join().unwrap();
    assert_eq!(
        hl_gpu::Decoder::stream(&first_body).unwrap(),
        upload,
        "first executor saw the upload"
    );
    let recovered = hl_gpu::Decoder::stream(&recovered).unwrap();
    assert_eq!(
        &recovered[..upload.len()],
        upload.as_slice(),
        "residency first, exactly once"
    );
    assert_eq!(
        &recovered[upload.len()..],
        draw.as_slice(),
        "new work follows the replay"
    );
    assert_eq!(
        sink.generation(),
        2,
        "one reconnect advanced the generation"
    );
    assert!(
        !sink.take_residency_reset(),
        "successful replay consumed the reset generation"
    );
}

/// What the acknowledgement tells the guest, pinned — because a layer above this one now decides the
/// BLAST RADIUS of a failure from it (`GlobalState::retires_share_group` in the GL shim fails only the
/// refused call, while every other transport kind still retires the share group).
///
/// The distinction that decision rests on is drawable here and always has been: a refusal is minted only
/// where a complete acknowledgement was read, and every other kind is minted from an I/O failure. The
/// byte's VALUE is not what carries it — the fact that an answer arrived at all is. A rejection therefore
/// proves the host received, decoded and declined the request, which is what makes it recoverable.
///
/// What the single-value acknowledgement does not carry is WHICH error caused the refusal: the host holds
/// a typed error and sends an undifferentiated no, so `acknowledgement` is 0 whatever the cause. This test
/// pins the scope of the guarantee; closing that gap is owned elsewhere.
#[test]
fn the_ack_separates_an_answer_from_silence_but_not_one_refusal_from_another() {
    use hl_gpu::{GpuError, TransportError};

    let sock = TempSock::new("ack-refusal");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::permissive_fixture("host");
        serve_connection(&stream, &caps, |_h, _b: &[Cmd]| false).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    let refused = sink.submit(&[Cmd::CreateFence(1)]).expect_err("refused");
    assert!(
        sink.terminal_error().is_none(),
        "a refusal must not retire the sink: {:?}",
        sink.terminal_error()
    );
    drop(sink);
    server.join().unwrap();

    let GpuError::Transport(refused) = refused else {
        panic!("a refusal must surface as a transport error");
    };
    assert!(
        refused.refusal(),
        "an answered request is a refusal, not a dead transport: {refused:?}"
    );
    assert_eq!(
        refused,
        TransportError::Rejected {
            phase: hl_gpu::TransportPhase::Acknowledgement,
            acknowledgement: 0,
        },
        "the host writes one failure value for every cause, so the reason is gone by here"
    );

    // The other half of the split — that a transport which never answered is NOT a refusal — is asserted
    // where the decision is made, in the GL shim's `only_an_unusable_transport_retires_the_group`, which
    // walks every kind. Driving an absent peer from here is not the cheap control it looks like: a submit
    // against a socket path with no listener did not return promptly in this harness, which deserves
    // attention on its own and is not this test's subject.
}
