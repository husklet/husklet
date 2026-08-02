use super::*;

type RecordedCommands = (
    Vec<Enc>,
    Vec<(u32, u64, Vec<u8>)>,
    Vec<DeferredOp>,
    Vec<VkBuffer>,
);

/// Replay executable secondary command buffers into a recording primary in submission order.
pub fn cmd_execute_commands(
    dev: &mut Device,
    primary: VkCommandBuffer,
    secondaries: &[VkCommandBuffer],
) -> Result<()> {
    let _ = dev.require_recording(primary)?;

    for &secondary in secondaries {
        match dev.command_buffers.get(&secondary) {
            Some(recording) if recording.state == CommandBufferState::Executable => {}
            _ => {
                return Err(GpuError::Invalid(
                    "vkCmdExecuteCommands: secondary is unknown or not executable",
                ));
            }
        }
    }

    let recordings: Vec<RecordedCommands> = secondaries
        .iter()
        .filter_map(|secondary| dev.command_buffers.get(secondary))
        .map(|recording| {
            (
                recording.enc.clone(),
                recording.buffer_writes.clone(),
                recording.deferred.clone(),
                recording.gpu_written_buffers.clone(),
            )
        })
        .collect();

    let primary = dev.require_recording(primary)?;
    for (encoder, writes, deferred, gpu_written_buffers) in recordings {
        primary.enc.extend(encoder);
        primary.buffer_writes.extend(writes);
        primary.deferred.extend(deferred);
        for buffer in gpu_written_buffers {
            if !primary.gpu_written_buffers.contains(&buffer) {
                primary.gpu_written_buffers.push(buffer);
            }
        }
    }
    Ok(())
}
