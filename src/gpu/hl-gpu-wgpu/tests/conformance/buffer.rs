use super::*;

#[test]
fn bound_adapter_is_software_vulkan() {
    let g = exec();
    let info = g.adapter_info();
    eprintln!(
        "wgpu adapter: name={:?} backend={:?} type={:?}",
        info.name, info.backend, info.device_type
    );
    #[cfg(target_os = "macos")]
    {
        // Real Apple GPU via Metal (no software fallback needed — the whole suite runs on the hardware).
        assert_eq!(
            info.backend,
            wgpu::Backend::Metal,
            "expected the Metal backend on macOS, got {:?}",
            info.name
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(
            info.backend,
            wgpu::Backend::Vulkan,
            "expected the Vulkan backend (lavapipe)"
        );
        let name = info.name.to_lowercase();
        assert!(
            name.contains("llvmpipe")
                || name.contains("lavapipe")
                || info.device_type == wgpu::DeviceType::Cpu,
            "expected a software adapter, got {:?}",
            info.name
        );
    }
}

// -------------------------------------------------------------------------------------------------
// buffer: write + readback
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_write_then_readback_exact_bytes() {
    let data = vec![0x01u8, 0x02, 0x03, 0x04, 0xAA, 0xBB, 0xCC, 0xDD];
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: data.clone(),
            },
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, data);
}

#[test]
fn buffer_write_at_offset_leaves_prefix_zeroed() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 4,
                data: vec![0x11, 0x22, 0x33, 0x44],
            },
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [0, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn buffer_partial_readback_window() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![0, 1, 2, 3, 4, 5, 6, 7],
            },
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 2, 3).unwrap();
    assert_eq!(out, [2, 3, 4]);
}

// -------------------------------------------------------------------------------------------------
// buffer -> buffer copy
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_to_buffer_copy_full() {
    let src = vec![0xDEu8, 0xAD, 0xBE, 0xEF];
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(out, src);
}

#[test]
fn buffer_to_buffer_copy_with_offsets() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 6).unwrap();
    assert_eq!(out, [0, 0, 0, 0, 12, 13]);
}
