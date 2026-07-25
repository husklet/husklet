use super::*;

// =================================================================================================
// 5. TWO-PASS COMPUTE — pass B reads the buffer pass A produced; the dependency is honored
// =================================================================================================

/// Two compute passes in ONE command buffer: pass A writes `mid[i] = src[i] + 100`, pass B reads `mid` and
/// writes `dst[i] = mid[i] * 2`. The executor runs each pass as its own submit+wait, so pass B observing
/// pass A's writes is the cross-pass dependency under test. Both `mid` (A's product) and `dst` (B's product,
/// which can only be right if it read A's output) are asserted bit-exact.
#[test]
fn two_pass_compute_dependency_honored() {
    const N: u32 = 500;
    let groups = N.div_ceil(64); // 8 groups → total 512, remainder 12 guarded
    let src_vals: Vec<u32> = (0..N).map(|i| i.wrapping_mul(13).wrapping_add(5)).collect();
    let mid_expect: Vec<u32> = src_vals.iter().map(|v| v.wrapping_add(100)).collect();
    let dst_expect: Vec<u32> = mid_expect.iter().map(|v| v.wrapping_mul(2)).collect();

    let pass_a = "\
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> mid: array<u32>;
const N: u32 = 500u;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < N) { mid[i] = src[i] + 100u; }
}";
    let pass_b = "\
@group(0) @binding(0) var<storage, read> mid: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
const N: u32 = 500u;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < N) { dst[i] = mid[i] * 2u; }
}";

    let sz = (N * 4) as u64;
    let mut g = exec();
    // Shaders/pipelines: 1 = pass A, 2 = pass B. Buffers: 1 = src, 2 = mid, 3 = dst. Bind groups: 1 = A's
    // (src, mid), 2 = B's (mid, dst) — each a single set-0 group matching its pipeline's auto layout.
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: wgsl_to_spirv(pass_a),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::SpirV,
                spirv: wgsl_to_spirv(pass_b),
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
            ),
            Cmd::CreateComputePipeline(
                2,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 2,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, sbuf(sz)),
            Cmd::CreateBuffer(2, sbuf(sz)),
            Cmd::CreateBuffer(3, sbuf(sz)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&src_vals),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![whole(0, 1, sz), whole(1, 2, sz)],
                },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc {
                    set: 0,
                    entries: vec![whole(0, 2, sz), whole(1, 3, sz)],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch {
                        x: groups,
                        y: 1,
                        z: 1,
                    },
                    Enc::EndComputePass,
                    Enc::BeginComputePass,
                    Enc::SetPipeline(2),
                    Enc::SetBindGroup { index: 0, group: 2 },
                    Enc::Dispatch {
                        x: groups,
                        y: 1,
                        z: 1,
                    },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    assert_eq!(
        read_u32s(&g, &s, 2, N as usize),
        mid_expect,
        "pass A must write mid = src + 100"
    );
    assert_eq!(read_u32s(&g, &s, 3, N as usize), dst_expect,
        "pass B must read pass A's mid and write dst = mid*2 — the cross-pass dependency is honored");
}
