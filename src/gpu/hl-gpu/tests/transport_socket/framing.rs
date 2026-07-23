use super::*;

#[test]
fn large_frame_roundtrip_many_buffers_byte_exact() {
    with_watchdog(30, || {
        // 8 buffers × 512 KiB = 4 MiB of uploads in ONE framed submit, each buffer a DISTINCT byte pattern,
        // plus a few thousand no-op creates so the frame is both multi-MiB AND many-command. Every buffer
        // must survive the OS socket buffering intact and read back byte-for-byte off the real CpuExecutor.
        const N_BUF: u32 = 8;
        const BUF_LEN: usize = 512 << 10; // 512 KiB each
        let sock = TempSock::new("large");
        let listener = UnixListener::bind(&sock.0).unwrap();
        let server = thread::spawn(move || serve_one(&listener).unwrap());

        // Distinct content per buffer: byte = (buffer_index * 31 + position) so no two buffers alias.
        let content = |b: u32| -> Vec<u8> {
            (0..BUF_LEN)
                .map(|i| (b as usize * 31 + i * 2654435761usize) as u8)
                .collect()
        };

        let mut batch = Vec::new();
        for b in 1..=N_BUF {
            let data = content(b);
            batch.push(Cmd::CreateBuffer(
                b,
                BufferDesc {
                    size: BUF_LEN as u64,
                    usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ));
            batch.push(Cmd::WriteBuffer {
                id: b,
                offset: 0,
                data,
            });
        }
        for f in 0..3000u32 {
            batch.push(Cmd::CreateFence(100_000 + f));
        }

        let mut sink = RemoteCommandSink::new(sock.path());
        sink.submit(&batch)
            .expect("multi-MiB frame acknowledged over the socket");

        // Read every buffer back over the SAME connection and assert byte-exact — and a partial window too.
        for b in 1..=N_BUF {
            let want = content(b);
            let got = sink
                .read_buffer(BufferId(b), 0, BUF_LEN)
                .expect("readback whole buffer");
            assert_eq!(
                got, want,
                "buffer {b} round-tripped byte-exact through the multi-MiB frame"
            );
            let mid = sink
                .read_buffer(BufferId(b), 1000, 256)
                .expect("partial readback");
            assert_eq!(mid, &want[1000..1256], "buffer {b} partial window is exact");
        }

        drop(sink);
        server.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------------
// 2. fragmented_send — a valid frame split across many tiny writes; the serve loop reassembles it
// ---------------------------------------------------------------------------------------------------

#[test]
fn fragmented_frame_reassembles_through_the_serve_loop() {
    with_watchdog(30, || {
        // The client hand-writes a valid submit frame ONE BYTE AT A TIME with a flush + brief pause between
        // bytes, so the server's blocking reader is forced to reassemble the frame across dozens of partial
        // reads (each `read` returns a fragment). The runtime host must execute the SAME program it would
        // from a single write — proven by reading the uploaded buffer back byte-exact afterward.
        let data: Vec<u8> = (0..777u32).map(|i| (i * 7 + 1) as u8).collect();
        let sock = TempSock::new("frag");
        let listener = UnixListener::bind(&sock.0).unwrap();
        let server = thread::spawn(move || serve_one(&listener).unwrap());

        let client = UnixStream::connect(&sock.0).unwrap();
        drain_handshake(&client);

        // Frame the submit ourselves so we control the byte-by-byte trickle.
        let payload = hl_gpu::Encoder::stream(&write_program(1, &data));
        let header = SubmitHeader {
            surface_id: 1,
            width: 0,
            height: 0,
            len: payload.len() as u32,
        };
        let mut wire = header.to_bytes().to_vec();
        wire.extend_from_slice(&payload);

        {
            let mut w = &client;
            for (i, byte) in wire.iter().enumerate() {
                w.write_all(&[*byte]).expect("trickle one byte");
                w.flush().ok();
                // Pause on the first bytes (spanning the header→payload boundary) and periodically after,
                // to force the reader to return short reads rather than coalescing the whole frame.
                if i < 24 || i % 97 == 0 {
                    thread::sleep(Duration::from_micros(200));
                }
            }
        }

        // The host acked the reassembled frame (ACK_OK), proving the fragmented submit decoded correctly.
        let mut ack = [0u8; 1];
        (&client)
            .read_exact(&mut ack)
            .expect("host acked the reassembled fragmented frame");
        assert_eq!(
            ack[0], ACK_OK,
            "a frame split across single-byte writes must reassemble + ACK"
        );

        // Read the uploaded buffer back: byte-exact == the fragmented submit was reassembled correctly.
        unix::Connection::new(&client)
            .write_readback_request(&ReadbackRequest::buffer(1, 0, data.len() as u64))
            .unwrap();
        let got = unix::Connection::new(&client)
            .read_readback_response()
            .expect("readback after fragmented submit");
        assert_eq!(
            got, data,
            "the fragmented frame produced the same result as a single write"
        );

        drop(client);
        server.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------------
// 3. disconnect_mid_frame — a partial frame then a dropped connection; graceful, and the next connection works
// ---------------------------------------------------------------------------------------------------

#[test]
fn disconnect_mid_frame_is_graceful_and_a_fresh_connection_works() {
    with_watchdog(30, || {
        // Connection #1: the client sends a header promising a payload, then only HALF the payload, then
        // drops. The serve loop must surface a clean typed error (truncated payload / UnexpectedEof) — no
        // hang, no panic, no corruption. Connection #2 (a fresh accept + fresh host) must then serve a real
        // submit + readback perfectly, proving the mid-frame disconnect did not wedge the listener.
        let sock = TempSock::new("disc");
        let listener = UnixListener::bind(&sock.0).unwrap();

        let server = thread::spawn(move || {
            // #1: expect a transport error from the truncated frame (peer vanished mid-payload).
            let r1 = serve_one(&listener);
            // #2: a fresh connection must serve cleanly to a clean EOF.
            let r2 = serve_one(&listener);
            (r1.is_err(), r2)
        });

        // #1: connect, consume handshake, promise 64 payload bytes but send only 20, then drop.
        {
            let client = UnixStream::connect(&sock.0).unwrap();
            drain_handshake(&client);
            let header = SubmitHeader {
                surface_id: 1,
                width: 0,
                height: 0,
                len: 64,
            };
            let mut w = &client;
            w.write_all(&header.to_bytes()).unwrap();
            w.write_all(&[0xAB; 20]).unwrap(); // 20 of the promised 64 -> truncated
            w.flush().ok();
            drop(client); // vanish mid-frame
        }

        // #2: a fresh, well-behaved connection round-trips a submit + readback.
        let data = vec![0x5Au8; 32];
        let mut sink = RemoteCommandSink::new(sock.path());
        sink.submit(&write_program(2, &data))
            .expect("fresh connection submits after the disconnect");
        let got = sink
            .read_buffer(BufferId(2), 0, data.len())
            .expect("fresh connection readback works");
        assert_eq!(
            got, data,
            "the connection AFTER a mid-frame disconnect is fully functional"
        );
        drop(sink);

        let (first_errored, second) = server.join().unwrap();
        assert!(
            first_errored,
            "a mid-frame disconnect surfaces a typed transport error, not a panic/hang"
        );
        assert!(
            second.is_ok(),
            "the serve loop recovered: the next connection served to a clean EOF"
        );
    });
}

// ---------------------------------------------------------------------------------------------------
// 4. concurrent_connections — several interleaved connections, each an isolated Session, no cross-bleed
// ---------------------------------------------------------------------------------------------------
