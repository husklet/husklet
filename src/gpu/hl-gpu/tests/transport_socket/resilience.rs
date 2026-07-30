use super::*;

#[test]
fn malformed_length_prefix_is_capped_without_oom_and_recovers() {
    with_watchdog(30, || {
        // A hostile client stamps a header with a 4 GiB (`u32::MAX`) declared payload — far past the
        // MAX_FRAME_BYTES DoS cap — then drops WITHOUT sending the promised body. The serve loop must refuse
        // to preallocate that buffer (the cap applies at the WIRE read), drain to resync, hit EOF, and
        // return a typed error — all in well under the watchdog and without exhausting memory. A fresh
        // connection afterward must still work. This is the DoS cap enforced at the serve loop, not just in
        // the adapter unit test.
        const {
            assert!(
                u32::MAX > MAX_FRAME_BYTES,
                "the forged length must exceed the transport cap"
            );
        }
        let sock = TempSock::new("malformed");
        let listener = UnixListener::bind(&sock.0).unwrap();

        let server = thread::spawn(move || {
            let r1 = serve_one(&listener); // the malformed-prefix connection
            let r2 = serve_one(&listener); // a fresh, well-behaved connection
            (r1.is_err(), r2)
        });

        // #1: forge the absurd length prefix, send NO body, drop.
        {
            let client = UnixStream::connect(&sock.0).unwrap();
            drain_handshake(&client);
            let header = SubmitHeader {
                surface_id: 1,
                width: 0,
                height: 0,
                len: u32::MAX,
            };
            let mut w = &client;
            w.write_all(&header.to_bytes()).unwrap();
            w.flush().ok();
            drop(client); // never send the 4 GiB it promised
        }

        // #2: a normal submit + readback proves the listener survived the malformed prefix.
        let data = vec![0xC3u8; 64];
        let mut sink = RemoteCommandSink::new(sock.path());
        sink.submit(&write_program(3, &data))
            .expect("fresh connection works after a malformed prefix");
        let got = sink
            .read_buffer(BufferId(3), 0, data.len())
            .expect("readback after malformed prefix");
        assert_eq!(
            got, data,
            "the connection after a malformed length prefix is fully functional"
        );
        drop(sink);

        let (first_errored, second) = server.join().unwrap();
        assert!(
            first_errored,
            "an over-cap wire length is refused (drained to EOF) as a typed error, not OOM"
        );
        assert!(
            second.is_ok(),
            "the serve loop recovered: the next connection served cleanly"
        );
    });
}

// ---------------------------------------------------------------------------------------------------
// 6. backpressure — a pipelined burst of many frames with acks drained concurrently: no deadlock
// ---------------------------------------------------------------------------------------------------

#[test]
fn pipelined_burst_does_not_deadlock_under_backpressure() {
    with_watchdog(45, || {
        // A raw client pipelines MANY frames back-to-back (a writer thread) while a separate reader thread
        // drains the acks, so more than a socket-buffer's worth of frame bytes is in flight at once and the
        // writer genuinely blocks waiting on the server to drain — real backpressure. The lockstep
        // per-frame ack must NOT deadlock: every frame is eventually processed in order, proven by a final
        // readback of the whole buffer equalling the concatenation of every chunk written.
        const FRAMES: usize = 200;
        const CHUNK: usize = 4096; // 200 × 4 KiB ≈ 800 KiB in flight, past the socket buffer
        let total = FRAMES * CHUNK;
        let sock = TempSock::new("backpressure");
        let listener = UnixListener::bind(&sock.0).unwrap();
        let server = thread::spawn(move || serve_one(&listener).unwrap());

        let client = UnixStream::connect(&sock.0).unwrap();
        drain_handshake(&client);

        // Frame 0: create the destination buffer. Wait for its ack before pipelining writes into it.
        {
            let create = hl_gpu::Encoder::stream(&[Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: total as u64,
                    usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            )]);
            let hdr = SubmitHeader {
                surface_id: 1,
                width: 0,
                height: 0,
                len: create.len() as u32,
            };
            unix::Connection::new(&client)
                .write_frame(&hdr, &create)
                .unwrap();
            let mut ack = [0u8; 1];
            (&client).read_exact(&mut ack).unwrap();
            assert_eq!(ack[0], ACK_OK, "buffer creation acked");
        }

        // Concurrent ack drainer: reads exactly FRAMES acks so the writer can never wedge on a full buffer.
        let ack_reader = {
            let client = client.try_clone().unwrap();
            thread::spawn(move || {
                let mut s = &client;
                let mut oks = 0usize;
                for _ in 0..FRAMES {
                    let mut ack = [0u8; 1];
                    s.read_exact(&mut ack).expect("ack for a pipelined frame");
                    if ack[0] == ACK_OK {
                        oks += 1;
                    }
                }
                oks
            })
        };

        // Writer: pipeline FRAMES WriteBuffer frames back-to-back WITHOUT waiting per-ack. Each frame writes
        // its chunk to a distinct offset with a value derived from the frame index, so the assembled buffer
        // is a known pattern. Backpressure engages when the in-flight bytes exceed the OS socket buffer.
        {
            let mut w = &client;
            for f in 0..FRAMES {
                let chunk = vec![(f as u8).wrapping_mul(3).wrapping_add(1); CHUNK];
                let batch = [Cmd::WriteBuffer {
                    id: 1,
                    offset: (f * CHUNK) as u64,
                    data: chunk,
                }];
                let payload = hl_gpu::Encoder::stream(&batch);
                let hdr = SubmitHeader {
                    surface_id: 1,
                    width: 0,
                    height: 0,
                    len: payload.len() as u32,
                };
                unix::Connection::new(w)
                    .write_frame(&hdr, &payload)
                    .expect("pipelined frame write");
                let _ = w.flush();
            }
        }

        let oks = ack_reader.join().expect("ack drainer thread");
        assert_eq!(
            oks, FRAMES,
            "every pipelined frame was processed and acked — no deadlock, no drop"
        );

        // Final readback of the WHOLE buffer proves ordered, complete processing of the burst.
        unix::Connection::new(&client)
            .write_readback_request(&ReadbackRequest::buffer(1, 0, total as u64))
            .unwrap();
        let got = unix::Connection::new(&client)
            .read_readback_response(total)
            .expect("full-buffer readback after the burst");
        assert_eq!(
            got.len(),
            total,
            "the whole buffer was written by the burst"
        );
        for f in 0..FRAMES {
            let want = (f as u8).wrapping_mul(3).wrapping_add(1);
            assert!(
                got[f * CHUNK..(f + 1) * CHUNK].iter().all(|&b| b == want),
                "frame {f}'s chunk landed intact and in order under backpressure"
            );
        }

        drop(client);
        server.join().unwrap();
    });
}
