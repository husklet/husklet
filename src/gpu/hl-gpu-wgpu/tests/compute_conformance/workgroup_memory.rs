use super::*;

// =================================================================================================
// 2. SHARED MEMORY + BARRIERS — a workgroup-local tree reduction, one result per workgroup
// =================================================================================================

/// A workgroup-local reduction (`var<workgroup>` scratch + `workgroupBarrier()` between tree-halving steps),
/// one result per workgroup, asserted bit-exact against a per-workgroup CPU reduction. The barrier is
/// load-bearing: without it a thread would read a neighbour's scratch slot before that neighbour's write
/// landed, and the reduced value would diverge — so an exact match across every workgroup proves the barrier
/// synchronizes the workgroup and that each workgroup's shared memory is isolated (a bleed from another
/// workgroup's scratch would corrupt the sum/max). Run for BOTH `+` (sum) and `max`.

#[test]
fn shared_memory_reduction_sum_and_max() {
    const WG: u32 = 64;
    const NUM_WG: u32 = 17; // not a power of two — the per-workgroup result vector is 17 wide
    let n = (WG * NUM_WG) as usize; // exact multiple → no guard needed, every thread has an element
    let input: Vec<u32> = (0..n as u32)
        .map(|i| i.wrapping_mul(31).wrapping_add(1) & 0xFFFF)
        .collect();

    // (label, WGSL combine op, WGSL identity, CPU fold).
    #[allow(clippy::type_complexity)]
    let variants: [(&str, &str, &str, fn(u32, u32) -> u32); 2] = [
        ("sum", "acc + scratch[lid.x + stride]", "0u", |a, b| {
            a.wrapping_add(b)
        }),
        ("max", "max(acc, scratch[lid.x + stride])", "0u", |a, b| {
            a.max(b)
        }),
    ];

    let mut g = exec();
    for (label, combine, _identity, fold) in &variants {
        let src = format!(
            "\
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
var<workgroup> scratch: array<u32, 64>;
@compute @workgroup_size(64)
fn cs_main(@builtin(local_invocation_id) lid: vec3<u32>,
           @builtin(workgroup_id) wid: vec3<u32>,
           @builtin(global_invocation_id) gid: vec3<u32>) {{
    scratch[lid.x] = input[gid.x];
    workgroupBarrier();
    var stride: u32 = 32u;
    loop {{
        if (stride == 0u) {{ break; }}
        if (lid.x < stride) {{
            let acc = scratch[lid.x];
            scratch[lid.x] = {combine};
        }}
        workgroupBarrier();
        stride = stride / 2u;
    }}
    if (lid.x == 0u) {{ output[wid.x] = scratch[0]; }}
}}"
        );

        // CPU reference: fold each workgroup's 64-element slice.
        let expect: Vec<u32> = (0..NUM_WG as usize)
            .map(|w| {
                input[w * WG as usize..(w + 1) * WG as usize]
                    .iter()
                    .copied()
                    .reduce(fold)
                    .unwrap()
            })
            .collect();

        let s = run_one(
            &mut g,
            &src,
            &[
                Buf {
                    id: 1,
                    init: u32s(&input),
                },
                Buf {
                    id: 2,
                    init: u32s(&vec![0u32; NUM_WG as usize]),
                },
            ],
            vec![
                whole(0, 1, (n * 4) as u64),
                whole(1, 2, (NUM_WG * 4) as u64),
            ],
            (NUM_WG, 1, 1),
        );
        let got = read_u32s(&g, &s, 2, NUM_WG as usize);
        assert_eq!(
            got, expect,
            "shared-memory {label} reduction: each workgroup's result must be the bit-exact per-workgroup \
             {label} (proves workgroupBarrier synchronizes and scratch is isolated per workgroup)"
        );
    }
}
