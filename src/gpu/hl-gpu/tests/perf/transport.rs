use super::*;

struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-perf-{tag}-{}-{:?}.sock",
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

struct RuntimeHost {
    session: Session,
    exec: CpuExecutor,
}
impl RuntimeHost {
    fn new() -> Self {
        let exec = CpuExecutor::new();
        let limits = Limits::from_capabilities(exec.capabilities());
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        Self { session, exec }
    }
}
impl ConnectionHandler for RuntimeHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        let frame_bytes = hl_gpu::Encoder::stream(batch).len();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
            Ok(_) => Verdict::Ack,
            Err(_) => Verdict::Nack,
        }
    }
    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.exec,
            BufferId(req.id),
            req.offset,
            req.len as usize,
        )
        .ok()
    }
}

// -------------------------------------------------------------------------------------------------
// 3. transport round-trip — submit+ack latency, and device->host readback latency + MB/s
// -------------------------------------------------------------------------------------------------

#[test]
fn perf_transport_submit_and_readback_latency() {
    let sock = TempSock::new("rt");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::permissive_fixture("host");
        let mut host = RuntimeHost::new();
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());

    // A readback buffer we can hammer: create + fill a 1 MiB buffer once.
    let rb_bytes = 1usize << 20;
    sink.submit(&[
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: rb_bytes as u64,
                usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![0x5Au8; rb_bytes],
        },
    ])
    .expect("readback buffer upload");

    // --- submit+ack latency: a tiny batch, over the already-open connection ---
    // Unique fence ids per submit so the persistent host session never rejects a duplicate create.
    for i in 0..20u32 {
        sink.submit(&[Cmd::CreateFence(1000 + i)])
            .expect("warmup submit");
    }
    let k = 300u32;
    let mut samples = Vec::with_capacity(k as usize);
    for i in 0..k {
        let batch = [Cmd::CreateFence(2000 + i)];
        let t = Instant::now();
        sink.submit(&batch).expect("timed submit");
        samples.push(t.elapsed());
    }
    let submit_mean = mean(&samples);
    let submit_med = median(samples);
    println!(
        "perf: transport submit+ack = {:.1} us mean, {:.1} us median ({} iters, small batch)",
        us(submit_mean),
        us(submit_med),
        k
    );

    // --- readback latency + throughput: read the 1 MiB buffer repeatedly ---
    for _ in 0..5 {
        let _ = sink
            .read_buffer(BufferId(1), 0, rb_bytes)
            .expect("warmup readback");
    }
    let kr = 50u32;
    let mut rb_samples = Vec::with_capacity(kr as usize);
    for _ in 0..kr {
        let t = Instant::now();
        let got = sink
            .read_buffer(BufferId(1), 0, rb_bytes)
            .expect("timed readback");
        rb_samples.push(t.elapsed());
        assert_eq!(got.len(), rb_bytes);
    }
    let rb_mean = mean(&rb_samples);
    let rb_med = median(rb_samples);
    let rb_mbps = (rb_bytes as f64 / (1024.0 * 1024.0)) / rb_mean.as_secs_f64();
    println!(
        "perf: transport readback = {:.1} us mean, {:.1} us median, {:.1} MB/s ({} iters, {} bytes)",
        us(rb_mean),
        us(rb_med),
        rb_mbps,
        kr,
        rb_bytes
    );

    drop(sink);
    server.join().unwrap();

    // Generous ceilings / floors: catch a hang or a total collapse, ignore variance.
    assert!(
        us(submit_med) < 500_000.0,
        "submit latency implausibly high: {} us",
        us(submit_med)
    );
    assert!(
        us(rb_med) < 2_000_000.0,
        "readback latency implausibly high: {} us",
        us(rb_med)
    );
    assert!(
        rb_mbps > 0.1,
        "readback throughput collapsed: {rb_mbps} MB/s"
    );
}

// -------------------------------------------------------------------------------------------------
// 4. in-process submit latency — the socket-free path, for comparison against (3)
// -------------------------------------------------------------------------------------------------
