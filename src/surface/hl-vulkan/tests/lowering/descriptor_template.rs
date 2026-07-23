use super::*;

fn buffer_info_bytes(buffer: u64, offset: u64, range: u64) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..8].copy_from_slice(&buffer.to_le_bytes());
    b[8..16].copy_from_slice(&offset.to_le_bytes());
    b[16..24].copy_from_slice(&range.to_le_bytes());
    b
}

/// Bind `set` at set index 0 and return the resulting `CreateBindGroup`'s entries.
fn bind_and_capture_entries(
    d: &mut Device,
    sink: &mut RecordingSink,
    set: u64,
) -> Vec<hl_gpu::protocol::model::descriptor::BindEntry> {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(d, sink, cb, 0, &[set], &[]).unwrap();
    d.end_command_buffer(cb).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateBindGroup(_, desc)] => desc.entries.clone(),
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

#[test]
fn descriptor_template_update_matches_direct_writes() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let in_buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let out_buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();

    let layout = d.create_descriptor_set_layout(vec![
        LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
        LayoutBinding {
            binding: 1,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
    ]);
    let pool = d.create_descriptor_pool(8);

    // Set A: two direct buffer writes (the reference path).
    let set_direct = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set_direct, 0, in_buf, 0, 1024).unwrap();
    create::update_descriptor_buffer(&mut d, set_direct, 1, out_buf, 256, 512).unwrap();

    // Set B: the SAME two writes applied via a descriptor update template + a packed data blob.
    let set_tmpl = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let template = create::create_descriptor_update_template(
        &mut d,
        VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
        vec![
            DescriptorTemplateEntry {
                dst_binding: 0,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
                offset: 0,
                stride: 24,
            },
            DescriptorTemplateEntry {
                dst_binding: 1,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
                offset: 24,
                stride: 24,
            },
        ],
    )
    .unwrap();
    let mut blob = Vec::new();
    blob.extend_from_slice(&buffer_info_bytes(in_buf, 0, 1024));
    blob.extend_from_slice(&buffer_info_bytes(out_buf, 256, 512));
    create::update_descriptor_set_with_template(&mut d, set_tmpl, template, &blob).unwrap();

    // Binding either set produces the identical IR bind-group entries.
    let direct = bind_and_capture_entries(&mut d, &mut sink, set_direct);
    let via_template = bind_and_capture_entries(&mut d, &mut sink, set_tmpl);
    assert_eq!(direct, via_template);
    // …and they resolve the two storage buffers to ir ids 1 & 2 with the written offsets/ranges.
    assert_eq!(via_template.len(), 2);
    assert!(matches!(
        via_template[0].resource,
        BindResource::Buffer {
            id: 1,
            offset: 0,
            size: 1024
        }
    ));
    assert!(matches!(
        via_template[1].resource,
        BindResource::Buffer {
            id: 2,
            offset: 256,
            size: 512
        }
    ));
}

#[test]
fn descriptor_template_rejects_non_descriptor_set_type_and_bad_handles() {
    let mut d = dev();
    // A push-descriptor template type (1) is a truthful feature-not-present, never a fake success.
    assert!(matches!(
        create::create_descriptor_update_template(&mut d, 1, vec![]),
        Err(GpuError::Unsupported(_))
    ));
    // Updating with an unknown template / set is a typed error.
    assert!(create::update_descriptor_set_with_template(&mut d, 0xdead, 0xbeef, &[]).is_err());
    // A blob too short for a declared entry is a truthful out-of-bounds, not a junk read.
    let tmpl = create::create_descriptor_update_template(
        &mut d,
        VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
        vec![DescriptorTemplateEntry {
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: vk_descriptor_type::UNIFORM_BUFFER,
            offset: 0,
            stride: 24,
        }],
    )
    .unwrap();
    let pool = d.create_descriptor_pool(1);
    let layout = d.create_descriptor_set_layout(vec![]);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, set, tmpl, &[0u8; 8]),
        Err(GpuError::OutOfBounds)
    ));
}
