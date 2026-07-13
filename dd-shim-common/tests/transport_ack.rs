//! Executor-transport acknowledgement semantics.
//!
//! Do not inspect implementation source here. A rendering test must execute an API, transport,
//! state transition, or pixel path and assert its observable result.

#[test]
fn executor_transport_rejects_a_failed_frame_acknowledgement() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let path = std::env::temp_dir().join(format!("dd-render-ack-{}-{}.sock", std::process::id(),
        std::thread::current().name().unwrap_or("test")));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind fake executor");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept shim transport");
        let mut header = [0u8; 16];
        stream.read_exact(&mut header).expect("read frame header");
        let len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).expect("read frame payload");
        stream.write_all(&[0]).expect("send executor failure ack");
    });

    let mut connection = dd_shim_common::transport::ExecConn::new(path.to_string_lossy().into_owned());
    let surface = dd_shim_common::transport::Surface { id: 7, generation: 0, width: 16, height: 9, stride: 64, fd: -1 };
    // `submit` validates the IR wire format before it opens the transport, so the frame must be a
    // well-formed encoded stream — otherwise the ack path under test is never reached. The behavioral
    // assertion is unchanged: the executor answers ack=0 (failure) and `submit` must surface an error.
    let ir = dd_gpu::ir::encode_stream(&[dd_gpu::ir::Cmd::CreateFence(1)]);
    let result = connection.submit(&surface, &ir);
    server.join().unwrap();
    let _ = std::fs::remove_file(path);
    assert!(result.is_err(), "ExecConn treated executor failure ack=0 as success");
}
