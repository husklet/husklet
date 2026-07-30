use super::*;

pub(super) fn wgpu_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

/// A CPU-oracle session, built from the named oracle fixture rather than widened here by hand: see
/// [`hl_gpu::Capabilities::oracle_session_fixture`] for which payloads and formats it admits and why.
fn cpu_session(exec: &CpuExecutor) -> Session {
    let caps = hl_gpu::Capabilities::oracle_session_fixture(&exec.capabilities());
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

pub(super) fn run_wgpu(exec: &mut WgpuExecutor, prog: &Prog) -> hl_gpu::Result<Vec<u8>> {
    if let Some((id, k)) = &prog.kernel {
        exec.define_kernel(*id, k.clone());
    }
    let mut s = wgpu_session(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, &prog.cmds)?;
    match prog.read {
        Read::Tex { id, .. } => exec.read_texture(&s.resources, id),
        Read::Buf { id, offset, len } => exec.read_buffer(&s.resources, BufferId(id), offset, len),
    }
}

pub(super) fn run_cpu(prog: &Prog) -> hl_gpu::Result<Vec<u8>> {
    let mut cpu = CpuExecutor::new();
    if let Some((id, k)) = &prog.kernel {
        cpu.define_kernel(*id, k.clone());
    }
    let mut s = cpu_session(&cpu);
    hl_gpu::runtime::submit(&mut s, &mut cpu, 0, &prog.cmds)?;
    match prog.read {
        Read::Tex { id, len } => {
            let mut out = vec![0u8; len];
            cpu.read_texture(&s.resources, TextureId(id), &mut out)?;
            Ok(out)
        }
        Read::Buf { id, offset, len } => {
            GpuExecutor::read_buffer(&cpu, &s.resources, BufferId(id), offset, len)
        }
    }
}

/// Compare two readback planes per byte within `tol`; on the first out-of-tolerance byte, return a
/// minimised description (the offending index + both values + the max observed per-byte delta).
pub(super) fn diff(cpu: &[u8], gpu: &[u8], tol: i16) -> Option<String> {
    if cpu.len() != gpu.len() {
        return Some(format!(
            "length mismatch: cpu={} gpu={}",
            cpu.len(),
            gpu.len()
        ));
    }
    let mut worst = 0i16;
    let mut first_bad: Option<usize> = None;
    for i in 0..cpu.len() {
        let d = (cpu[i] as i16 - gpu[i] as i16).abs();
        if d > worst {
            worst = d;
        }
        if d > tol && first_bad.is_none() {
            first_bad = Some(i);
        }
    }
    first_bad.map(|i| {
        format!(
            "byte {i} (texel {}, chan {}) cpu={} gpu={} (tol {tol}, worst delta {worst})",
            i / 4,
            i % 4,
            cpu[i],
            gpu[i]
        )
    })
}
