use super::*;

#[test]
fn workgroup_sizes_all_agree_and_respect_bounds() {
    const N: u32 = 1000;
    const PAD: u32 = 64; // >= the largest per-config remainder (96-block leaves 56)
    const SENTINEL: u32 = 0xDEAD_BEEF;

    // Deterministic input; wrapping CPU reference for the in-range slots, sentinel for the padded tail.
    let src: Vec<u32> = (0..N).map(|i| i.wrapping_mul(7).wrapping_add(3)).collect();
    let mut expect: Vec<u32> = src
        .iter()
        .map(|v| v.wrapping_mul(3).wrapping_add(7))
        .collect();
    expect.extend(std::iter::repeat(SENTINEL).take(PAD as usize));
    let out_len = (N + PAD) as usize;
    let dst_init = u32s(&vec![SENTINEL; out_len]);

    // (label, WGSL body computing the linear index `i`, dispatch groups). Each grid is a bijection onto
    // `[0, total)` with `N <= total <= N+PAD`, so exactly the remainder `[N, total)` is out-of-range.
    let head = "\
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
const N: u32 = 1000u;
";
    let configs: [(&str, String, (u32, u32, u32)); 6] = [
        // 1D, workgroup_size(1): over-dispatch to 1024 groups → 24 out-of-range invocations.
        (
            "ws1",
            format!(
                "{head}
@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"
            ),
            (1024, 1, 1),
        ),
        // 1D, workgroup_size(64): 16 groups → total 1024, remainder 24.
        (
            "ws64",
            format!(
                "{head}
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"
            ),
            (16, 1, 1),
        ),
        // 1D, workgroup_size(256): 4 groups → total 1024, remainder 24.
        (
            "ws256",
            format!(
                "{head}
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"
            ),
            (4, 1, 1),
        ),
        // 1D, NON-power-of-2 workgroup_size(96): 11 groups → total 1056, remainder 56.
        (
            "ws96",
            format!(
                "{head}
@compute @workgroup_size(96)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"
            ),
            (11, 1, 1),
        ),
        // 2D block (8,8), dispatch (4,4) → 32x32 grid, row-major linear index, total 1024, remainder 24.
        (
            "ws2d_8x8",
            format!(
                "{head}
@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.y * 32u + gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"
            ),
            (4, 4, 1),
        ),
        // 3D block (4,4,4), dispatch (2,4,2) → 8x16x8 grid, z-major linear index, total 1024, remainder 24.
        (
            "ws3d_4x4x4",
            format!(
                "{head}
@compute @workgroup_size(4, 4, 4)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = (gid.z * 16u + gid.y) * 8u + gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"
            ),
            (2, 4, 2),
        ),
    ];

    let mut g = exec();
    for (label, src_wgsl, dispatch) in &configs {
        let s = run_one(
            &mut g,
            src_wgsl,
            &[
                Buf {
                    id: 1,
                    init: u32s(&src),
                },
                Buf {
                    id: 2,
                    init: dst_init.clone(),
                },
            ],
            vec![
                whole(0, 1, (N * 4) as u64),
                whole(1, 2, (out_len * 4) as u64),
            ],
            *dispatch,
        );
        let got = read_u32s(&g, &s, 2, out_len);
        assert_eq!(
            got, expect,
            "workgroup config {label}: elementwise map must be bit-exact on [0,N) AND leave the padded tail \
             at the sentinel (guarded out-of-range invocations wrote nothing)"
        );
    }
}
