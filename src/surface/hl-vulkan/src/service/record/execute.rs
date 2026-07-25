use super::*;

type RecordedCommands = (Vec<Enc>, Vec<(u32, u64, Vec<u8>)>, Vec<DeferredOp>);

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
            )
        })
        .collect();

    let primary = dev.require_recording(primary)?;
    for (encoder, writes, deferred) in recordings {
        primary.enc.extend(encoder);
        primary.buffer_writes.extend(writes);
        primary.deferred.extend(deferred);
    }
    Ok(())
}
