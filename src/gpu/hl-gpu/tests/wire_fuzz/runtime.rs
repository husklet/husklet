use super::*;

/// A fresh per-connection session over the CPU reference oracle (the full advertised IR surface).
fn fresh_session() -> (Session, CpuExecutor) {
    let exec = CpuExecutor::new();
    let caps = exec.capabilities();
    let limits = Limits::from_capabilities(caps);
    let session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    (session, exec)
}

/// Decode a byte stream and run it through the WHOLE runtime pipeline against the oracle. Any stage's
/// typed error propagates; nothing here may panic on decodable-but-hostile input.
fn decode_then_submit(bytes: &[u8]) -> Result<()> {
    let cmds = hl_gpu::Decoder::stream(bytes)?;
    let (mut session, mut exec) = fresh_session();
    hl_gpu::runtime::submit(&mut session, &mut exec, bytes.len(), &cmds)?;
    Ok(())
}

#[test]
fn duplicate_id_in_a_decoded_stream_is_typed_duplicate_id() {
    // Two creates of the same fence id — a self-referential/duplicate stream. account rejects the
    // re-create over a still-live id as DuplicateId before charging (failure-atomic), the same typed
    // error the executor's id table would raise, end-to-end from bytes.
    let bytes = hl_gpu::Encoder::stream(&[Cmd::CreateFence(1), Cmd::CreateFence(1)]);
    assert_eq!(
        decode_then_submit(&bytes),
        Err(GpuError::DuplicateId {
            kind: "fence",
            id: 1
        })
    );
}

#[test]
fn dangling_id_in_a_decoded_stream_is_typed_unknown_id() {
    // Destroy a buffer that was never created (a dangling reference) — a typed UnknownId, not a panic.
    let bytes = hl_gpu::Encoder::stream(&[Cmd::DestroyBuffer(7)]);
    assert_eq!(
        decode_then_submit(&bytes),
        Err(GpuError::UnknownId {
            kind: "buffer",
            id: 7
        })
    );
}

#[test]
fn absurd_buffer_size_in_a_decoded_stream_is_resource_limit_not_oom() {
    // A CreateBuffer declaring u64::MAX bytes is rejected at VALIDATE against the negotiated per-buffer
    // ceiling — BEFORE the executor ever attempts `vec![0u8; size]` (which would OOM-abort the host).
    let bytes = hl_gpu::Encoder::stream(&[Cmd::CreateBuffer(
        1,
        BufferDesc {
            size: u64::MAX,
            usage: 0,
            label: String::new(),
        },
    )]);
    assert_eq!(
        decode_then_submit(&bytes),
        Err(GpuError::ResourceLimit("buffer bytes"))
    );
}

#[test]
fn oversized_texture_footprint_in_a_decoded_stream_is_resource_limit() {
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    use hl_gpu::protocol::model::enums::{TextureDim, TextureFormat};
    // A texture whose declared dimensions exceed the negotiated 2D ceiling is a typed ResourceLimit — the
    // footprint is never materialized. Dims over max_texture_2d (16384 for the oracle).
    let bytes = hl_gpu::Encoder::stream(&[Cmd::CreateTexture(
        2,
        TextureDesc {
            width: 100_000,
            height: 100_000,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: 0,
            label: String::new(),
        },
    )]);
    assert_eq!(
        decode_then_submit(&bytes),
        Err(GpuError::ResourceLimit("texture dimensions"))
    );
}

#[test]
fn the_whole_pipeline_never_panics_on_hostile_bytes() {
    // Random bytes and bit-flip mutations of a valid stream driven through decode → validate → account →
    // dispatch: decode either rejects them or the runtime returns a typed error — the pipeline never
    // panics, hangs, or OOM-aborts. (The existing fuzz corpora stop at decode_stream; this covers the
    // runtime stages too.)
    let base = hl_gpu::Encoder::stream(&[
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 256,
                usage: 0x3F,
                label: "b".into(),
            },
        ),
        Cmd::CreateFence(2),
        Cmd::DestroyFence(2),
        Cmd::DestroyBuffer(1),
    ]);
    // The base stream itself runs clean end-to-end.
    assert!(
        decode_then_submit(&base).is_ok(),
        "the well-formed base stream must submit cleanly"
    );

    let mut state = 0xDEAD_BEEF_0BAD_F00Du64;
    for _ in 0..20_000u32 {
        // Half random bytes, half mutated-valid stream.
        let bytes = if lcg(&mut state) & 1 == 0 {
            let len = (lcg(&mut state) as usize) % 200;
            let mut b = Vec::with_capacity(len);
            for _ in 0..len {
                b.push(lcg(&mut state));
            }
            b
        } else {
            let mut b = base.clone();
            let muts = 1 + (lcg(&mut state) % 4) as usize;
            for _ in 0..muts {
                if b.is_empty() {
                    break;
                }
                let pos = (lcg(&mut state) as usize) % b.len();
                b[pos] = lcg(&mut state);
            }
            b
        };
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = decode_then_submit(&bytes);
        }));
        assert!(
            r.is_ok(),
            "decode→submit PANICKED on {} bytes: {:02x?}",
            bytes.len(),
            bytes
        );
    }
}
