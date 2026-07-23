use super::*;

#[test]
fn perf_inprocess_submit_latency() {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    // Distinct fence ids so validation never rejects a duplicate create.
    for i in 0..20u32 {
        sink.submit(&[Cmd::CreateFence(i + 1)])
            .expect("warmup submit");
    }
    let k = 500u32;
    let mut samples = Vec::with_capacity(k as usize);
    for i in 0..k {
        let batch = [Cmd::CreateFence(100 + i)];
        let t = Instant::now();
        sink.submit(&batch).expect("timed submit");
        samples.push(t.elapsed());
    }
    let m = mean(&samples);
    let med = median(samples);
    println!(
        "perf: in-process submit = {:.2} us mean, {:.2} us median ({} iters, small batch)",
        us(m),
        us(med),
        k
    );
    assert!(
        us(med) < 100_000.0,
        "in-process submit latency implausibly high: {} us",
        us(med)
    );
}
