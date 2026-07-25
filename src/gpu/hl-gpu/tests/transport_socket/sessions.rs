use super::*;

#[test]
fn concurrent_connections_have_isolated_sessions() {
    with_watchdog(30, || {
        // N clients connect at once. Each writes buffer id=1 (the SAME id on every connection) with a value
        // unique to that client, then reads id=1 back. If any resources bled across connections, a reader
        // would see another client's bytes; isolation means each reads back EXACTLY its own value. The
        // server spawns one RuntimeHost (a fresh Session + CpuExecutor) per accepted connection.
        const N: usize = 8;
        const LEN: usize = 4096;
        let sock = TempSock::new("concurrent");
        let listener = UnixListener::bind(&sock.0).unwrap();

        let server = thread::spawn(move || {
            let caps = Capabilities::full("host");
            let mut handles = Vec::new();
            for stream in listener.incoming().take(N) {
                let stream = stream.unwrap();
                let caps = caps.clone();
                handles.push(thread::spawn(move || {
                    let mut host = RuntimeHost::new();
                    // A per-connection error must not abort the others; each thread owns its result.
                    let _ = serve_connection_with_handler(&stream, &caps, &mut host);
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
        });

        let path = sock.path();
        let mut clients = Vec::new();
        for k in 0..N {
            let path = path.clone();
            clients.push(thread::spawn(move || {
                let value = (0x40 + k) as u8; // a byte value unique to this connection
                let data = vec![value; LEN];
                let mut sink = RemoteCommandSink::new(path);
                sink.submit(&write_program(1, &data))
                    .expect("per-connection submit");
                let got = sink
                    .read_buffer(BufferId(1), 0, LEN)
                    .expect("per-connection readback");
                // Every byte must be THIS connection's value — never another connection's.
                assert!(
                    got.iter().all(|&b| b == value),
                    "connection {k} saw foreign bytes — cross-connection resource bleed"
                );
                assert_eq!(got.len(), LEN);
            }));
        }
        for c in clients {
            c.join()
                .expect("a concurrent client thread panicked (isolation broken)");
        }
        server.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------------
// 5. malformed_length_prefix — a bogus/huge wire length is capped at the read, no OOM, connection recovers
// ---------------------------------------------------------------------------------------------------
