use super::*;

// =================================================================================================
// 3. ATOMICS — many invocations racing one storage location; the final value is the race-free answer
// =================================================================================================

/// Thousands of invocations hammer a SINGLE atomic storage location with `atomicAdd` / `atomicMax` /
/// `atomicMin`, and a fourth kernel proves `atomicExchange` conserves values. Each final value equals the
/// race-free arithmetic answer computed on the CPU — an atomic that dropped even one update (a lost
/// read-modify-write, not serialized) would fall short. This is the direct proof that the executor's compute
/// path serializes atomics correctly on lavapipe.

#[test]
fn atomics_serialize_add_max_min_exchange() {
    const WG: u32 = 64;
    const GROUPS: u32 = 64;
    let t: u32 = WG * GROUPS; // 4096 racing invocations

    // A deterministic per-invocation value, matched in WGSL and on the CPU (kept < 2^24 so max/min are lively
    // and no sum overflows u32).
    let hval = |g: u32| g.wrapping_mul(2_654_435_761) & 0x00FF_FFFF;

    let mut g = exec();

    // --- atomicAdd: final == sum_{g<T}(g+1) == T*(T+1)/2 (exact race-free total) ---
    {
        let src = format!(
            "\
@group(0) @binding(0) var<storage, read_write> counter: atomic<u32>;
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    atomicAdd(&counter, gid.x + 1u);
}}"
        );
        let s = run_one(
            &mut g,
            &src,
            &[Buf {
                id: 1,
                init: u32s(&[0]),
            }],
            vec![whole(0, 1, 4)],
            (GROUPS, 1, 1),
        );
        let expect = (0..t).fold(0u32, |a, k| a.wrapping_add(k + 1));
        assert_eq!(
            read_u32s(&g, &s, 1, 1)[0],
            expect,
            "atomicAdd of {t} invocations must sum to the exact race-free total (no lost update)"
        );
    }

    // --- atomicMax: final == max_{g<T} hval(g) ---
    {
        let src = format!(
            "\
@group(0) @binding(0) var<storage, read_write> m: atomic<u32>;
fn h(g: u32) -> u32 {{ return (g * 2654435761u) & 0x00FFFFFFu; }}
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    atomicMax(&m, h(gid.x));
}}"
        );
        let s = run_one(
            &mut g,
            &src,
            &[Buf {
                id: 1,
                init: u32s(&[0]),
            }],
            vec![whole(0, 1, 4)],
            (GROUPS, 1, 1),
        );
        let expect = (0..t).map(hval).max().unwrap();
        assert_eq!(
            read_u32s(&g, &s, 1, 1)[0],
            expect,
            "atomicMax must settle on the true maximum across all invocations"
        );
    }

    // --- atomicMin: final == min_{g<T} hval(g), starting from u32::MAX ---
    {
        let src = format!(
            "\
@group(0) @binding(0) var<storage, read_write> m: atomic<u32>;
fn h(g: u32) -> u32 {{ return (g * 2654435761u) & 0x00FFFFFFu; }}
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    atomicMin(&m, h(gid.x));
}}"
        );
        let s = run_one(
            &mut g,
            &src,
            &[Buf {
                id: 1,
                init: u32s(&[u32::MAX]),
            }],
            vec![whole(0, 1, 4)],
            (GROUPS, 1, 1),
        );
        let expect = (0..t).map(hval).min().unwrap();
        assert_eq!(
            read_u32s(&g, &s, 1, 1)[0],
            expect,
            "atomicMin must settle on the true minimum across all invocations"
        );
    }

    // --- atomicExchange conservation: sum(out[]) + final_slot == sum_{g<T}(g+1) ---
    // Each invocation swaps `gid+1` into the slot and records the displaced old value. Every value that ever
    // occupied the slot (the 0 seed, plus every injected `gid+1`) is either later displaced out (recorded in
    // `out`) or remains as the final slot value — so the two multisets are equal and their sums match. A
    // dropped or duplicated exchange would break the invariant, so this proves exchange serializes.
    {
        let src = format!(
            "\
@group(0) @binding(0) var<storage, read_write> slot: atomic<u32>;
@group(0) @binding(1) var<storage, read_write> out: array<u32>;
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    out[gid.x] = atomicExchange(&slot, gid.x + 1u);
}}"
        );
        let s = run_one(
            &mut g,
            &src,
            &[
                Buf {
                    id: 1,
                    init: u32s(&[0]),
                },
                Buf {
                    id: 2,
                    init: u32s(&vec![0u32; t as usize]),
                },
            ],
            vec![whole(0, 1, 4), whole(1, 2, (t * 4) as u64)],
            (GROUPS, 1, 1),
        );
        let final_slot = read_u32s(&g, &s, 1, 1)[0] as u64;
        let out_sum: u64 = read_u32s(&g, &s, 2, t as usize)
            .iter()
            .map(|&v| v as u64)
            .sum();
        let expect: u64 = (0..t as u64).map(|k| k + 1).sum();
        assert_eq!(out_sum + final_slot, expect,
            "atomicExchange must conserve every value (displaced-out sum + residue == injected sum): \
             proves exchanges serialize with no lost/duplicated swap");
    }
}
