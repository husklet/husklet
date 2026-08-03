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

/// The format of the plane a program reads back, derived from the `CreateTexture` that made it.
///
/// Derived rather than declared beside the read. A second declaration is a second thing to keep in step,
/// and every drift found in this driver tonight was two places describing one fact; this one cannot
/// disagree with the texture the program actually creates. `None` means the read is not a texture, or
/// names one this program never created — the comparator treats that as a reason to refuse, not as a
/// reason to assume bytes.
pub(super) fn read_plane(prog: &Prog) -> Option<TextureFormat> {
    let Read::Tex { id, .. } = prog.read else {
        return None;
    };
    prog.cmds.iter().rev().find_map(|cmd| match cmd {
        Cmd::CreateTexture(created, desc) if *created == id => Some(desc.format),
        _ => None,
    })
}

/// Map a float's bits to a monotonic integer, so that adjacent representable values are adjacent
/// integers and the distance between two of them is a count of ULPs.
///
/// IEEE bit patterns already sort correctly for positives; negatives sort backwards, so their magnitude
/// is negated. Both zeroes map to 0, which is what makes `-0.0` and `+0.0` zero ULPs apart rather than a
/// full sign-bit's distance.
fn ordered(bits: u64, sign: u64) -> i64 {
    if bits & sign != 0 {
        -((bits & !sign) as i64)
    } else {
        bits as i64
    }
}

/// Decode one channel of a float plane to its raw bits and its value, or `None` for a non-float plane.
fn float_channels(plane: TextureFormat, bytes: &[u8]) -> Option<(Vec<(u64, f32)>, u64)> {
    let (width, sign): (usize, u64) = match plane {
        TextureFormat::Rgba16Float => (2, 0x8000),
        TextureFormat::Rgba32Float | TextureFormat::R32Float => (4, 0x8000_0000),
        _ => return None,
    };
    let channels = bytes
        .chunks_exact(width)
        .map(|c| {
            if width == 2 {
                let bits = u16::from_le_bytes([c[0], c[1]]);
                (bits as u64, hl_gpu::protocol::model::half::to_f32(bits))
            } else {
                let bits = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                (bits as u64, f32::from_bits(bits))
            }
        })
        .collect();
    Some((channels, sign))
}

/// Compare two readback planes within `tol`, in the units the plane has. On the first channel outside
/// tolerance, return a minimised description naming the channel, both values and the worst distance seen.
///
/// A FLOAT plane is decoded and compared in ULPs of its own encoding. Comparing its bytes was not merely
/// imprecise, it was meaningless: a one-ULP difference that carries across a byte boundary shows as a
/// per-byte delta of 255, so the byte view cannot distinguish "the last mantissa bit disagrees" from "the
/// exponent is wrong". Two NaNs compare equal because no arithmetic here distinguishes payloads; a NaN
/// against a number is always a divergence.
pub(super) fn diff(
    cpu: &[u8],
    gpu: &[u8],
    tol: Tolerance,
    plane: Option<TextureFormat>,
) -> Option<String> {
    if cpu.len() != gpu.len() {
        return Some(format!(
            "length mismatch: cpu={} gpu={}",
            cpu.len(),
            gpu.len()
        ));
    }
    match tol {
        Tolerance::Ulps(tol) => {
            // A ULP tolerance on a plane whose format could not be determined, or which is not a float
            // plane, is a comparison nobody defined. Refuse rather than fall back to bytes and report a
            // number that looks like a measurement.
            let Some(plane) = plane else {
                return Some(
                    "a ULP tolerance needs the plane's format and this program's read does not name a \
                     texture it created"
                        .into(),
                );
            };
            let (Some((cpu, sign)), Some((gpu, _))) =
                (float_channels(plane, cpu), float_channels(plane, gpu))
            else {
                return Some(format!(
                    "a ULP tolerance is only defined for a float plane; this one is {plane:?}"
                ));
            };
            let mut worst = 0i64;
            let mut first_bad = None;
            for (index, ((cb, cv), (gb, gv))) in cpu.iter().zip(gpu.iter()).enumerate() {
                let bad;
                let distance;
                if cv.is_nan() || gv.is_nan() {
                    distance = 0;
                    bad = cv.is_nan() != gv.is_nan();
                } else {
                    distance = (ordered(*cb, sign) - ordered(*gb, sign)).abs();
                    bad = distance > tol as i64;
                }
                worst = worst.max(distance);
                if bad && first_bad.is_none() {
                    first_bad = Some((index, *cv, *gv, distance));
                }
            }
            first_bad.map(|(index, cv, gv, distance)| {
                format!(
                    "channel {index} cpu={cv:e} gpu={gv:e} ({distance} ulp, tol {tol} ulp, \
                     worst {worst} ulp, {plane:?})"
                )
            })
        }
        Tolerance::Unorm(tol) => {
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
    }
}

// -------------------------------------------------------------------------------------------------
// The comparator's own contract. These need no adapter and run with the rest of the binary.
// -------------------------------------------------------------------------------------------------

/// Build a half-float plane from raw bit patterns.
#[cfg(test)]
fn halves(bits: &[u16]) -> Vec<u8> {
    bits.iter().flat_map(|b| b.to_le_bytes()).collect()
}

/// The case that made a value comparator necessary: one ULP that carries across a byte boundary.
///
/// `0x0100` against `0x00FF` are adjacent representable halves — one ULP apart — but their low bytes are
/// `0x00` and `0xFF`, so the per-byte view calls them 255 apart. That is why widening the byte tolerance
/// was never the cheap fix: the only byte tolerance that admits this pair also admits a wrong exponent.
#[test]
fn one_ulp_across_a_byte_boundary_is_one_ulp_not_two_hundred_and_fifty_five() {
    let a = halves(&[0x0100]);
    let b = halves(&[0x00FF]);

    // The byte view, stated so the reason this comparator exists is checkable rather than asserted.
    assert_eq!(
        (a[0] as i16 - b[0] as i16).abs(),
        255,
        "byte-wise these differ by 255"
    );

    let plane = Some(TextureFormat::Rgba16Float);
    assert!(
        diff(&a, &b, Tolerance::Ulps(1), plane).is_none(),
        "value-wise they are one ULP apart and within a one-ULP tolerance"
    );
    let exact =
        diff(&a, &b, Tolerance::Ulps(0), plane).expect("one ULP exceeds an exact tolerance");
    assert!(
        exact.contains("1 ulp"),
        "and the report says how far apart they are in ULPs: {exact}"
    );
}

/// A one-ULP tolerance must not admit a wrong exponent. This is the half of the argument that makes the
/// tolerance meaningful rather than merely small: the pair above differs in one byte by 255 and passes,
/// this pair differs in one byte by 4 and must fail.
#[test]
fn a_one_ulp_tolerance_still_refuses_a_wrong_exponent() {
    let plane = Some(TextureFormat::Rgba16Float);
    // 0x3c00 is 1.0; 0x4000 is 2.0 — one exponent apart, 1024 ULPs.
    let a = halves(&[0x3c00]);
    let b = halves(&[0x4000]);
    assert_eq!(
        (a[1] as i16 - b[1] as i16).abs(),
        4,
        "byte-wise a small delta"
    );
    let report = diff(&a, &b, Tolerance::Ulps(1), plane).expect("a doubled value is not one ULP");
    assert!(
        report.contains("1024 ulp"),
        "the report names the real distance: {report}"
    );
}

/// Signed zeroes are the same value, and two NaNs agree while a NaN against a number does not.
#[test]
fn zeroes_agree_across_sign_and_nan_only_agrees_with_nan() {
    let plane = Some(TextureFormat::Rgba16Float);
    assert!(
        diff(
            &halves(&[0x0000]),
            &halves(&[0x8000]),
            Tolerance::Ulps(0),
            plane
        )
        .is_none(),
        "-0.0 and +0.0 are zero ULPs apart, not a sign bit apart"
    );
    assert!(
        diff(
            &halves(&[0x7e00]),
            &halves(&[0x7c01]),
            Tolerance::Ulps(0),
            plane
        )
        .is_none(),
        "two NaNs agree; no arithmetic here distinguishes payloads"
    );
    assert!(
        diff(
            &halves(&[0x7e00]),
            &halves(&[0x3c00]),
            Tolerance::Ulps(0),
            plane
        )
        .is_some(),
        "a NaN against a number is always a divergence"
    );
}

/// A ULP tolerance whose plane cannot be identified, or which is not a float plane, REFUSES rather than
/// quietly falling back to a byte comparison.
///
/// A fallback here would be the worst kind: it would produce a plausible pass or fail in a unit nobody
/// asked for, and the caller could not tell that the comparison it requested never happened.
#[test]
fn a_ulp_tolerance_refuses_a_plane_it_cannot_compare() {
    let bytes = vec![0u8; 8];
    let unknown = diff(&bytes, &bytes, Tolerance::Ulps(0), None)
        .expect("an unidentified plane cannot be compared in ULPs");
    assert!(unknown.contains("format"), "and says why: {unknown}");

    let wrong = diff(
        &bytes,
        &bytes,
        Tolerance::Ulps(0),
        Some(TextureFormat::Rgba8Unorm),
    )
    .expect("a byte plane has no ULPs");
    assert!(wrong.contains("Rgba8Unorm"), "and names it: {wrong}");

    // The control: the same identical planes DO compare clean under the tolerance they belong to, so the
    // refusals above are about the unit and not about the comparator failing everything.
    assert!(
        diff(
            &bytes,
            &bytes,
            Tolerance::Unorm(0),
            Some(TextureFormat::Rgba8Unorm)
        )
        .is_none(),
        "identical byte planes agree exactly"
    );
}
