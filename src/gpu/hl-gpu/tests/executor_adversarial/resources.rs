use super::*;

#[test]
fn empty_batch_is_a_clean_noop() {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    assert_eq!(
        exec.execute(&mut res, &[]).unwrap(),
        hl_gpu::Execution::accepted(vec![])
    );
    assert_eq!(res.live_count(), 0);
}

#[test]
fn duplicate_create_is_typed_duplicate_id() {
    let (mut exec, mut res) = primed(&[Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST))]);
    let err = exec
        .execute(
            &mut res,
            &[Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST))],
        )
        .unwrap_err();
    assert_eq!(
        err,
        GpuError::DuplicateId {
            kind: "buffer",
            id: 1
        }
    );
}

#[test]
fn destroy_unknown_is_typed_unknown_id() {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    assert_eq!(
        exec.execute(&mut res, &[Cmd::DestroyBuffer(99)])
            .unwrap_err(),
        GpuError::UnknownId {
            kind: "buffer",
            id: 99
        }
    );
}

#[test]
fn use_after_free_is_typed_unknown_id() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
        Cmd::DestroyBuffer(1),
    ]);
    let err = exec
        .execute(
            &mut res,
            &[Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![1, 2, 3, 4],
            }],
        )
        .unwrap_err();
    assert_eq!(
        err,
        GpuError::UnknownId {
            kind: "buffer",
            id: 1
        }
    );
    // Double free is likewise typed.
    assert_eq!(
        exec.execute(&mut res, &[Cmd::DestroyBuffer(1)])
            .unwrap_err(),
        GpuError::UnknownId {
            kind: "buffer",
            id: 1
        }
    );
}

// ---------------------------------------------------------------------------------------------------
// bounds checks: write / read / fill / copy OutOfBounds (never corruption or panic)
// ---------------------------------------------------------------------------------------------------

#[test]
fn write_buffer_out_of_bounds_is_rejected() {
    let (mut exec, mut res) = primed(&[Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST))]);
    // offset + len overruns the 4-byte buffer.
    let err = exec
        .execute(
            &mut res,
            &[Cmd::WriteBuffer {
                id: 1,
                offset: 2,
                data: vec![0, 0, 0, 0],
            }],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
    // A write starting past the end also fails, even with empty data past the end.
    assert_eq!(
        exec.execute(
            &mut res,
            &[Cmd::WriteBuffer {
                id: 1,
                offset: 5,
                data: vec![]
            }]
        )
        .unwrap_err(),
        GpuError::OutOfBounds
    );
}

#[test]
fn read_buffer_out_of_bounds_is_rejected() {
    let (exec, res) = primed(&[Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST))]);
    let mut out = [0u8; 4];
    // Reading 4 bytes at offset 2 runs off the end.
    assert_eq!(
        exec.read_buffer(&res, BufferId(1), 2, &mut out)
            .unwrap_err(),
        GpuError::OutOfBounds
    );
    // Reading a live buffer fully at the boundary is fine (offset 0, len 4).
    assert!(exec.read_buffer(&res, BufferId(1), 0, &mut out).is_ok());
    // Reading a non-existent buffer is a typed error.
    assert_eq!(
        exec.read_buffer(&res, BufferId(2), 0, &mut out)
            .unwrap_err(),
        GpuError::UnknownId {
            kind: "buffer",
            id: 2
        }
    );
}

#[test]
fn fill_buffer_out_of_bounds_is_rejected_atomically() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![0xFF; 8],
        },
    ]);
    // size overruns [offset, offset+size) past the 8-byte buffer.
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::FillBuffer {
                buffer: 1,
                offset: 4,
                size: 8,
                value: 0,
            }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
    // Failure atomicity: the buffer is untouched (validation runs before any write).
    let mut out = [0u8; 8];
    exec.read_buffer(&res, BufferId(1), 0, &mut out).unwrap();
    assert_eq!(out, [0xFF; 8], "a rejected fill mutated nothing");
}

#[test]
fn copy_buffer_to_buffer_out_of_bounds_is_rejected() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
    ]);
    // size 8 exceeds the 4-byte source.
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyBufferToBuffer {
                src: 1,
                src_offset: 0,
                dst: 2,
                dst_offset: 0,
                size: 8,
            }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}

#[test]
fn copy_missing_usage_flag_is_typed_invalid() {
    // A copy source without the COPY_SRC usage bit is rejected as Invalid (not OOB / not a silent copy).
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST)), // no COPY_SRC
        Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
    ]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyBufferToBuffer {
                src: 1,
                src_offset: 0,
                dst: 2,
                dst_offset: 0,
                size: 4,
            }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::Invalid("copy src lacks COPY_SRC"));
}

#[test]
fn copy_from_unknown_buffer_is_typed_unknown_id() {
    let (mut exec, mut res) = primed(&[Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST))]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyBufferToBuffer {
                src: 77,
                src_offset: 0,
                dst: 2,
                dst_offset: 0,
                size: 4,
            }])],
        )
        .unwrap_err();
    assert_eq!(
        err,
        GpuError::UnknownId {
            kind: "buffer",
            id: 77
        }
    );
}

#[test]
fn copy_texture_to_texture_region_out_of_bounds_is_rejected() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateTexture(
            1,
            tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC),
        ),
        Cmd::CreateTexture(
            2,
            tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_DST),
        ),
    ]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyTextureToTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d { x: 2, y: 2, z: 0 }, // origin + extent runs past the 4x4 plane
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                extent: Extent3d {
                    width: 4,
                    height: 4,
                    depth: 1,
                },
            }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}

// ---------------------------------------------------------------------------------------------------
// encoder-state validation: open passes, nesting, unbound draw/dispatch
// ---------------------------------------------------------------------------------------------------
