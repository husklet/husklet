use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::model::external_memory::{ExternalMemory, ImportedMemory, Mapping};
use hl_gpu::{BufferId, Cmd, CommandSink, ExportId, GpuError, Result};

pub fn import(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    export: ExportId,
    requested_bytes: u64,
) -> Result<ExternalMemory> {
    if requested_bytes == 0 {
        return Err(GpuError::Invalid(
            "CUDA external memory size must be non-zero",
        ));
    }
    let buffer = BufferId(ctx.alloc_buffer());
    let authoritative = sink.import_buffer(buffer, export)?;
    if authoritative != requested_bytes {
        let _ = sink.submit(&[Cmd::DestroyBuffer(buffer.0)]);
        return Err(GpuError::Invalid(
            "CUDA external memory size does not match export",
        ));
    }
    Ok(ctx.external_memories.insert(ImportedMemory {
        buffer,
        export,
        bytes: authoritative,
        mapping: Mapping::None,
    }))
}

pub fn mapped_buffer(
    ctx: &mut CudaContext,
    memory: ExternalMemory,
    offset: u64,
    size: u64,
) -> Result<DevicePtr> {
    let entry = ctx
        .external_memories
        .get(memory)
        .copied()
        .ok_or(GpuError::Invalid("CUDA external memory handle"))?;
    if entry.mapping != Mapping::None
        || size == 0
        || offset.checked_add(size).is_none_or(|end| end > entry.bytes)
    {
        return Err(GpuError::Invalid("CUDA external memory mapped range"));
    }
    let pointer = ctx.mem.insert_alias(entry.buffer.0, size, offset);
    ctx.external_memories.get_mut(memory).unwrap().mapping = Mapping::Live(pointer);
    Ok(pointer)
}

pub fn destroy(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    memory: ExternalMemory,
) -> Result<()> {
    let entry = ctx
        .external_memories
        .get(memory)
        .copied()
        .ok_or(GpuError::Invalid("CUDA external memory handle"))?;
    if matches!(entry.mapping, Mapping::Live(_)) {
        return Err(GpuError::Invalid(
            "CUDA external memory still has a mapped buffer",
        ));
    }
    if entry.mapping == Mapping::None {
        sink.submit(&[Cmd::DestroyBuffer(entry.buffer.0)])?;
    }
    ctx.external_memories.remove(memory);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CudaDeviceDesc;
    use hl_gpu::{Capabilities, FeatureRequest, FenceId};

    struct Sink {
        bytes: u64,
        imports: Vec<(BufferId, ExportId)>,
        batches: Vec<Vec<Cmd>>,
    }
    impl CommandSink for Sink {
        fn negotiate(&mut self, _: &FeatureRequest) -> Result<Capabilities> {
            Ok(Capabilities::permissive_fixture("external-memory-test"))
        }
        fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
            self.batches.push(batch.to_vec());
            Ok(())
        }
        fn wait(&mut self, _: FenceId, _: u64) -> Result<()> {
            Ok(())
        }
        fn import_buffer(&mut self, id: BufferId, export: ExportId) -> Result<u64> {
            self.imports.push((id, export));
            Ok(self.bytes)
        }
    }
    fn context() -> CudaContext {
        CudaContext::new(CudaDeviceDesc::apple_default(1 << 30))
    }

    #[test]
    fn import_requires_the_authoritative_exact_size() {
        let mut ctx = context();
        let mut sink = Sink {
            bytes: 64,
            imports: Vec::new(),
            batches: Vec::new(),
        };
        assert!(import(&mut ctx, &mut sink, ExportId(7), 63).is_err());
        assert!(matches!(
            sink.batches.last().unwrap().as_slice(),
            [Cmd::DestroyBuffer(_)]
        ));
        let memory = import(&mut ctx, &mut sink, ExportId(7), 64).unwrap();
        assert_eq!(ctx.external_memories.get(memory).unwrap().bytes, 64);
    }

    #[test]
    fn mapped_range_is_bounded_once_and_resolves_with_offset() {
        let mut ctx = context();
        let mut sink = Sink {
            bytes: 64,
            imports: Vec::new(),
            batches: Vec::new(),
        };
        let memory = import(&mut ctx, &mut sink, ExportId(7), 64).unwrap();
        assert!(mapped_buffer(&mut ctx, memory, 60, 8).is_err());
        assert!(mapped_buffer(&mut ctx, memory, u64::MAX, 2).is_err());
        let pointer = mapped_buffer(&mut ctx, memory, 16, 32).unwrap();
        assert_eq!(ctx.resolve(pointer), Some((sink.imports[0].0, 16)));
        assert_eq!(
            ctx.resolve(DevicePtr(pointer.0 + 31)),
            Some((sink.imports[0].0, 47))
        );
        assert!(mapped_buffer(&mut ctx, memory, 0, 1).is_err());
        assert!(destroy(&mut ctx, &mut sink, memory).is_err());
    }

    #[test]
    fn mapped_range_writes_its_full_view_at_the_backing_offset() {
        let mut ctx = context();
        let mut sink = Sink {
            bytes: 64,
            imports: Vec::new(),
            batches: Vec::new(),
        };
        let memory = import(&mut ctx, &mut sink, ExportId(7), 64).unwrap();
        let pointer = mapped_buffer(&mut ctx, memory, 8, 16).unwrap();
        let pattern = [0x5a; 16];
        crate::service::transfer::memcpy_htod(&mut ctx, &mut sink, pointer, &pattern).unwrap();
        assert!(matches!(
            sink.batches.last().unwrap().as_slice(),
            [Cmd::WriteBuffer { offset: 8, data, .. }] if data == &pattern
        ));

        let tail = [0x6b; 12];
        crate::service::transfer::memcpy_htod(
            &mut ctx,
            &mut sink,
            DevicePtr(pointer.0 + 4),
            &tail,
        )
        .unwrap();
        assert!(matches!(
            sink.batches.last().unwrap().as_slice(),
            [Cmd::WriteBuffer { offset: 12, data, .. }] if data == &tail
        ));

        assert!(crate::service::transfer::memcpy_htod(
            &mut ctx,
            &mut sink,
            DevicePtr(pointer.0 + 4),
            &[0; 13],
        )
        .is_err());
        assert!(crate::service::transfer::memcpy_htod(
            &mut ctx,
            &mut sink,
            DevicePtr(pointer.0 + 15),
            &[0; 2],
        )
        .is_err());
        assert!(crate::service::transfer::memcpy_htod(
            &mut ctx,
            &mut sink,
            DevicePtr(u64::MAX),
            &[0],
        )
        .is_err());
    }

    #[test]
    fn mapped_pointer_must_be_freed_before_external_handle() {
        let mut ctx = context();
        let mut sink = Sink {
            bytes: 64,
            imports: Vec::new(),
            batches: Vec::new(),
        };
        let memory = import(&mut ctx, &mut sink, ExportId(7), 64).unwrap();
        let pointer = mapped_buffer(&mut ctx, memory, 0, 64).unwrap();
        crate::service::allocate::mem_free(&mut ctx, &mut sink, pointer).unwrap();
        assert_eq!(
            ctx.external_memories.get(memory).unwrap().mapping,
            Mapping::Freed
        );
        destroy(&mut ctx, &mut sink, memory).unwrap();
        assert!(ctx.external_memories.get(memory).is_none());
        assert!(destroy(&mut ctx, &mut sink, memory).is_err());
    }
}
