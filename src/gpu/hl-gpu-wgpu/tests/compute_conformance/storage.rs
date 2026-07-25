use super::*;

// =================================================================================================
// 4. STORAGE READ-MODIFY-WRITE — in-place transform across a multi-workgroup dispatch
// =================================================================================================

/// Each invocation reads its own storage element, transforms it (`x*x + i`), and writes it back — a
/// multi-workgroup, non-power-of-2-count dispatch with a guarded remainder. The bit-exact in-place result on
/// `[0, N)` plus the untouched sentinel tail proves the RMW is correct at every element and that the
/// remainder invocations (i >= N) write nothing.
#[test]
fn storage_rmw_in_place_multi_workgroup() {
    const N: u32 = 1500; // not a multiple of 64 → remainder invocations
    const PAD: u32 = 64;
    const SENTINEL: u32 = 0x0BAD_F00D;
    let out_len = (N + PAD) as usize;

    let mut data: Vec<u32> = (0..N)
        .map(|i| i.wrapping_mul(2_246_822_519).wrapping_add(11) & 0xFFFF)
        .collect();
    let mut expect: Vec<u32> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| v.wrapping_mul(v).wrapping_add(i as u32))
        .collect();
    data.extend(std::iter::repeat_n(SENTINEL, PAD as usize));
    expect.extend(std::iter::repeat_n(SENTINEL, PAD as usize));

    let src = "\
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
const N: u32 = 1500u;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < N) {
        let v = data[i];
        data[i] = v * v + i;
    }
}";
    let groups = N.div_ceil(64); // 24 groups → total 1536, remainder 36 (< PAD)

    let mut g = exec();
    let s = run_one(
        &mut g,
        src,
        &[Buf {
            id: 1,
            init: u32s(&data),
        }],
        vec![whole(0, 1, (out_len * 4) as u64)],
        (groups, 1, 1),
    );
    let got = read_u32s(&g, &s, 1, out_len);
    assert_eq!(got, expect,
        "storage RMW must transform every element bit-exact across the multi-workgroup dispatch and leave \
         the guarded remainder tail untouched");
}
