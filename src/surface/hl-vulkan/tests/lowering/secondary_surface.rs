use super::*;

#[test]
fn execute_commands_replays_secondary_ops_into_primary() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let (s, t) = (buf_ir(&d, src), buf_ir(&d, dst));

    // A secondary records a copy (encoder op) + a fill (buffer write), then becomes Executable.
    let secondary = d.allocate_command_buffer();
    d.begin_command_buffer(secondary, false).unwrap();
    record::cmd_copy_buffer(&mut d, secondary, src, dst, 0, 0, 64).unwrap();
    record::cmd_fill_buffer(&mut d, secondary, dst, 128, 8, 0x0202_0202).unwrap();
    d.end_command_buffer(secondary).unwrap();

    // The primary executes the secondary, then is submitted.
    let primary = d.allocate_command_buffer();
    d.begin_command_buffer(primary, false).unwrap();
    record::cmd_execute_commands(&mut d, primary, &[secondary]).unwrap();
    d.end_command_buffer(primary).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[primary], None).unwrap();

    // The primary's submit carries the secondary's copy (encoder) preceded by the spliced fill (write).
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer {
            id,
            offset: 128,
            data,
        }, Cmd::Submit(cbuf)] => {
            assert_eq!(*id, t);
            assert_eq!(data, &vec![2u8; 8]);
            assert_eq!(
                cbuf.encoder,
                vec![Enc::CopyBufferToBuffer {
                    src: s,
                    src_offset: 0,
                    dst: t,
                    dst_offset: 0,
                    size: 64
                }]
            );
        }
        other => panic!("expected [WriteBuffer, Submit], got {other:?}"),
    }

    // A secondary that is not Executable (still recording) is a typed error, splicing nothing.
    let unfinished = d.allocate_command_buffer();
    d.begin_command_buffer(unfinished, false).unwrap();
    let p2 = d.allocate_command_buffer();
    d.begin_command_buffer(p2, false).unwrap();
    assert!(record::cmd_execute_commands(&mut d, p2, &[unfinished]).is_err());
}

// ---------------------------------------------------------------------------------------------------
// WSI physical-device surface queries: modeled caps / formats / present modes
// ---------------------------------------------------------------------------------------------------

#[test]
fn surface_queries_report_modeled_values() {
    // Support: only the lone present family (0) presents.
    assert!(present::QueueFamily(0).supports_present());
    assert!(!present::QueueFamily(1).supports_present());

    // Capabilities: double/triple-buffered, surface-defined extent, identity/opaque.
    let caps = present::surface_capabilities();
    assert_eq!(caps.min_image_count, 2);
    assert_eq!(caps.max_image_count, 3);
    assert_eq!(caps.current_extent, (u32::MAX, u32::MAX));
    assert_eq!(caps.max_image_extent, (16384, 16384));
    assert_eq!(caps.max_image_array_layers, 1);

    // Formats: BGRA8 + RGBA8, UNORM + SRGB, all SRGB-nonlinear.
    let formats = present::surface_formats();
    use hl_vulkan::model::queue::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR;
    assert!(formats
        .iter()
        .all(|f| f.color_space == VK_COLOR_SPACE_SRGB_NONLINEAR_KHR));
    assert!(formats.iter().any(|f| f.format == vk_format::B8G8R8A8_SRGB));
    assert!(formats.iter().any(|f| f.format == vk_format::R8G8B8A8_SRGB));

    // Present modes: FIFO (always-available) plus MAILBOX/IMMEDIATE, which real apps assume.
    use hl_vulkan::model::queue::{
        VK_PRESENT_MODE_FIFO_KHR, VK_PRESENT_MODE_IMMEDIATE_KHR, VK_PRESENT_MODE_MAILBOX_KHR,
    };
    let modes = present::surface_present_modes();
    assert_eq!(modes[0], VK_PRESENT_MODE_FIFO_KHR);
    assert!(modes.contains(&VK_PRESENT_MODE_MAILBOX_KHR));
    assert!(modes.contains(&VK_PRESENT_MODE_IMMEDIATE_KHR));
}
