use super::*;
use hl_gpu::{Capabilities, CommandSink, ExportId, FeatureRequest, FenceId, Result as GpuResult};

struct ExportSink {
    batches: Vec<Vec<Cmd>>,
    exports: Vec<BufferId>,
}
impl CommandSink for ExportSink {
    fn negotiate(&mut self, _: &FeatureRequest) -> GpuResult<Capabilities> {
        Ok(Capabilities::permissive_fixture(
            "vulkan-external-memory-test",
        ))
    }
    fn submit(&mut self, batch: &[Cmd]) -> GpuResult<()> {
        self.batches.push(batch.to_vec());
        Ok(())
    }
    fn wait(&mut self, _: FenceId, _: u64) -> GpuResult<()> {
        Ok(())
    }
    fn export_buffer(&mut self, id: BufferId) -> GpuResult<ExportId> {
        self.exports.push(id);
        Ok(ExportId(77))
    }
}

// =====================================================================================================
// memory: bind-offset flush + readback math (the suballocated-buffer path)
// =====================================================================================================

#[test]
fn external_memory_export_requires_one_exact_dedicated_buffer() {
    let mut d = dev();
    let mut s = ExportSink {
        batches: Vec::new(),
        exports: Vec::new(),
    };
    let buffer =
        create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let memory = d.allocate_memory(64).unwrap();
    assert!(create::export_memory_buffer(&d, &mut s, memory).is_err());
    d.set_memory_export_handle_types(memory, 1).unwrap();
    create::bind_buffer_memory(&mut d, buffer, memory, 0).unwrap();
    assert_eq!(
        create::export_memory_buffer(&d, &mut s, memory).unwrap(),
        ExportId(77)
    );
    assert_eq!(s.exports, vec![BufferId(buf_ir(&d, buffer))]);

    let offset_memory = d.allocate_memory(65).unwrap();
    d.set_memory_export_handle_types(offset_memory, 1).unwrap();
    let offset_buffer =
        create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    create::bind_buffer_memory(&mut d, offset_buffer, offset_memory, 1).unwrap();
    assert!(create::export_memory_buffer(&d, &mut s, offset_memory).is_err());
    d.map_memory(memory).unwrap();
    assert!(create::export_memory_buffer(&d, &mut s, memory).is_err());
}

/// REGRESSION: a persistently-mapped buffer bound at a NON-ZERO offset into its allocation must flush the
/// buffer's own footprint (`data[bound_offset..bound_offset+size]`), NOT the arena from offset 0. Before
/// the fix the still-mapped flush shipped `data[0..size]` — the wrong bytes for any suballocated buffer.

#[test]
fn still_mapped_flush_honors_bind_offset() {
    let mut d = dev();
    let mut s = sink();
    // 32-byte allocation; a 16-byte buffer bound at offset 16 (a second suballocation in one arena).
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = d.allocate_memory(32).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 16).unwrap();
    d.map_memory(mem).unwrap();
    // The app writes the buffer's bytes through the mapped pointer at allocation offset 16.
    let pattern: Vec<u8> = (1..=16u8).collect();
    create::write_mapped(&mut d, mem, 16, &pattern).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();

    // Exactly one WriteBuffer for our buffer, at buffer offset 0, carrying the FOOTPRINT bytes.
    let batch = s.batches.last().unwrap();
    let write = batch
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .expect("a WriteBuffer for the mapped buffer");
    assert_eq!(write.0, 0, "flush targets buffer offset 0");
    assert_eq!(
        write.1, pattern,
        "flush carries the buffer footprint, not the arena from offset 0"
    );
}

/// The device→host readback (`vkMapMemory`) also reads from the buffer footprint: a buffer bound at
/// offset 16, mapped over the whole allocation, issues a `read_buffer(ir, 0, 16)` — buffer-relative.
#[test]
fn read_mapped_bind_offset_reads_buffer_relative_range() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = d.allocate_memory(32).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 16).unwrap();
    d.map_memory(mem).unwrap();
    // Map the whole allocation (offset 0, WHOLE_SIZE); only the [16,32) footprint overlaps the buffer.
    create::read_mapped(&mut d, &mut s, mem, 0, u64::MAX).unwrap();
    assert_eq!(
        s.reads,
        vec![(BufferId(ir), 0, 16)],
        "readback is buffer-relative from footprint start"
    );
}

/// A pending (unmapped-before-submit) upload of a suballocated buffer flushes buffer-relative too.
#[test]
fn pending_flush_bind_offset_targets_buffer_relative_offset() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = d.allocate_memory(32).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 16).unwrap();
    d.map_memory(mem).unwrap();
    let pattern: Vec<u8> = (100..=115u8).collect();
    create::write_mapped(&mut d, mem, 16, &pattern).unwrap();
    d.unmap_memory(mem); // captures pending (0, WHOLE) intersected with footprint

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let write = s
        .batches
        .last()
        .unwrap()
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .expect("pending flush WriteBuffer");
    assert_eq!(write, (0, pattern));
}

/// A pending upload is one-shot: it flushes at the first submit and is retired, so a SECOND submit emits
/// no WriteBuffer for it (the app's staged bytes reach the device exactly once).
#[test]
fn pending_upload_is_cleared_after_one_submit() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 8).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = d.allocate_memory(8).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[9u8; 8]).unwrap();
    d.unmap_memory(mem);

    for expect_write in [true, false] {
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
        let has = s
            .batches
            .last()
            .unwrap()
            .iter()
            .any(|c| matches!(c, Cmd::WriteBuffer { id, .. } if *id == ir));
        assert_eq!(has, expect_write, "pending upload flushes exactly once");
    }
}

/// `capture_pending_upload` widens an already-pending sub-range so an earlier flush is never lost when a
/// second `vkFlushMappedMemoryRanges` covers a different span.
#[test]
fn flush_ranges_widen_and_cover_both_writes() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = d.allocate_memory(16).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();
    let all: Vec<u8> = (1..=16u8).collect();
    create::write_mapped(&mut d, mem, 0, &all).unwrap();
    // Two disjoint sub-range flushes: [0,4) then [12,16). The union must reach [0,16).
    create::capture_pending_upload(&mut d, mem, 0, 4);
    create::capture_pending_upload(&mut d, mem, 12, 4);
    d.unmap_memory(mem); // still-mapped? no — unmap keeps pending and widens to whole

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let (off, data) = s
        .batches
        .last()
        .unwrap()
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(off, 0);
    // Covers both ends (byte 0 and byte 15 present with their written values).
    assert_eq!(data[0], 1);
    assert_eq!(data[data.len() - 1], 16);
}

#[test]
fn write_mapped_out_of_range_is_out_of_bounds() {
    let mut d = dev();
    let mem = d.allocate_memory(8).unwrap();
    let err = create::write_mapped(&mut d, mem, 4, &[0u8; 8]).unwrap_err();
    assert!(matches!(err, GpuError::OutOfBounds));
}

#[test]
fn map_and_write_unknown_memory_error() {
    let mut d = dev();
    assert!(matches!(d.map_memory(0xdead), Err(GpuError::Invalid(_))));
    assert!(matches!(
        create::write_mapped(&mut d, 0xdead, 0, &[0]),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn bind_unknown_memory_or_buffer_errors_and_read_of_unbound_is_noop() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 16).unwrap();
    assert!(matches!(
        create::bind_buffer_memory(&mut d, buf, 0xdead, 0),
        Err(GpuError::Invalid(_))
    ));
    let mem = d.allocate_memory(16).unwrap();
    assert!(matches!(
        create::bind_buffer_memory(&mut d, 0xdead, mem, 0),
        Err(GpuError::Invalid(_))
    ));
    // read_mapped on host-only staging (no bound buffer) issues no readback and no error.
    d.map_memory(mem).unwrap();
    create::read_mapped(&mut d, &mut s, mem, 0, u64::MAX).unwrap();
    assert!(
        s.reads.is_empty(),
        "unbound staging has no device source to read back"
    );
}

// =====================================================================================================
// command-buffer lifecycle invariants
// =====================================================================================================
