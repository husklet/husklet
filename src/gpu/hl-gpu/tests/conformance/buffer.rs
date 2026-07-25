use super::*;

#[test]
fn buffer_write_then_readback_exact_bytes() {
    let data = vec![0x01u8, 0x02, 0x03, 0x04, 0xAA, 0xBB, 0xCC, 0xDD];
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: data.clone(),
            },
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, data.as_slice());
}

#[test]
fn buffer_write_at_offset_leaves_prefix_zeroed() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 4,
                data: vec![0x11, 0x22, 0x33, 0x44],
            },
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn buffer_partial_readback_window() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![0, 1, 2, 3, 4, 5, 6, 7],
            },
        ],
    );
    let mut out = [0u8; 3];
    exec.read_buffer(&s.resources, BufferId(1), 2, &mut out)
        .unwrap();
    assert_eq!(out, [2, 3, 4]);
}

// -------------------------------------------------------------------------------------------------
// buffer -> buffer copy
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_to_buffer_copy_full() {
    let src = vec![0xDEu8, 0xAD, 0xBE, 0xEF];
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 0,
                    dst: 2,
                    dst_offset: 0,
                    size: 4,
                }],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 4];
    exec.read_buffer(&s.resources, BufferId(2), 0, &mut out)
        .unwrap();
    assert_eq!(out, src.as_slice());
}

#[test]
fn buffer_to_buffer_copy_with_offsets() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(6, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(6, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![10, 11, 12, 13, 14, 15],
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 2,
                    dst: 2,
                    dst_offset: 4,
                    size: 2,
                }],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 6];
    exec.read_buffer(&s.resources, BufferId(2), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0, 0, 0, 0, 12, 13]);
}

// -------------------------------------------------------------------------------------------------
// texture clear + readback (render-pass clear and ClearRect)
// -------------------------------------------------------------------------------------------------

#[test]
fn fill_buffer_writes_repeating_pattern() {
    // Fill an 8-byte buffer with the little-endian pattern of 0xAABBCCDD, then read it back: the pattern
    // tiles from the fill offset (LE bytes DD CC BB AA repeated).
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 0,
                    size: 8,
                    value: 0xAABB_CCDD,
                }],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0xDD, 0xCC, 0xBB, 0xAA, 0xDD, 0xCC, 0xBB, 0xAA]);
}

#[test]
fn fill_buffer_scopes_to_offset_and_size() {
    // A sub-range fill must leave the bytes outside [offset, offset+size) untouched, and a size that is
    // not a multiple of 4 fills a partial pattern at the tail.
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![0xFF; 8],
            },
            Cmd::Submit(CommandBuffer {
                // fill bytes [2, 5): three bytes, pattern DD CC BB tiled from the region start.
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 2,
                    size: 3,
                    value: 0xAABB_CCDD,
                }],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0xFF, 0xFF, 0xDD, 0xCC, 0xBB, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn fill_buffer_round_trips_through_codec() {
    // The new encoder op survives encode → decode unchanged (additive wire round-trip).
    let cmds = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::FillBuffer {
            buffer: 7,
            offset: 16,
            size: 64,
            value: 0x1234_5678,
        }],
        signal: None,
    })];
    assert_eq!(
        hl_gpu::Decoder::stream(&hl_gpu::Encoder::stream(&cmds)).unwrap(),
        cmds
    );
}
