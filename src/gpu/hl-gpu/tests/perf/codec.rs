use super::*;

#[test]
fn perf_codec_encode_decode_throughput() {
    let cmds = representative_stream();
    let bytes = hl_gpu::Encoder::stream(&cmds).len();
    let mb = bytes as f64 / (1024.0 * 1024.0);

    // Warm up (fill caches / branch predictors).
    for _ in 0..3 {
        let e = hl_gpu::Encoder::stream(&cmds);
        let _ = hl_gpu::Decoder::stream(&e).unwrap();
    }

    let iters = 50u32;

    let t0 = Instant::now();
    let mut last = Vec::new();
    for _ in 0..iters {
        last = hl_gpu::Encoder::stream(&cmds);
    }
    let enc_elapsed = t0.elapsed();
    let enc_mbps = (mb * iters as f64) / enc_elapsed.as_secs_f64();

    let t1 = Instant::now();
    for _ in 0..iters {
        let _ = hl_gpu::Decoder::stream(&last).unwrap();
    }
    let dec_elapsed = t1.elapsed();
    let dec_mbps = (mb * iters as f64) / dec_elapsed.as_secs_f64();

    println!("perf: codec encode = {enc_mbps:.1} MB/s ({iters} iters, {bytes} bytes/stream)");
    println!("perf: codec decode = {dec_mbps:.1} MB/s ({iters} iters, {bytes} bytes/stream)");

    // Loose floors: a hang or a catastrophic regression fails; normal variance passes.
    assert!(
        bytes > 512 * 1024,
        "stream should be a few MB, got {bytes} bytes"
    );
    assert!(
        enc_mbps > 1.0,
        "encode throughput collapsed: {enc_mbps} MB/s"
    );
    assert!(
        dec_mbps > 1.0,
        "decode throughput collapsed: {dec_mbps} MB/s"
    );
}

// -------------------------------------------------------------------------------------------------
// 2. CPU compute throughput — a real vecadd over 1M f32 via InProcessCommandSink<CpuExecutor>
// -------------------------------------------------------------------------------------------------
