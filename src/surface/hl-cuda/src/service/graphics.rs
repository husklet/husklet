//! CUDA graphics-resource lifecycle over hl-GPU's explicit export/import access gate.

use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::model::graphics::{GraphicsBuffer, GraphicsResource};
use hl_gpu::{BufferId, Cmd, CommandSink, ExportId, GpuError, Result};

pub fn register_buffer(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    export: ExportId,
) -> Result<GraphicsResource> {
    let buffer = BufferId(ctx.alloc_buffer());
    let bytes = sink.import_buffer(buffer, export)?;
    let pointer = ctx.mem.insert(buffer.0, bytes);
    Ok(ctx.graphics.insert(GraphicsBuffer { buffer, export, pointer, bytes, mapped: false, map_flags: 0 }))
}

/// Set NONE, READ_ONLY, or WRITE_DISCARD for a registered, currently-unmapped resource.
pub fn set_map_flags(ctx: &mut CudaContext, resource: GraphicsResource, flags: u32) -> Result<()> {
    if flags > 2 { return Err(GpuError::Invalid("CUDA graphics map flags")); }
    let entry = ctx.graphics.get_mut(resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
    if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource still mapped")); }
    entry.map_flags = flags;
    Ok(())
}

pub fn map_resources(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    resources: &[GraphicsResource],
) -> Result<()> {
    validate_distinct(resources)?;
    for resource in resources {
        let entry = ctx.graphics.get(*resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
        if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource already mapped")); }
    }
    let mut mapped = Vec::new();
    for resource in resources {
        let buffer = ctx.graphics.get(*resource).unwrap().buffer;
        if let Err(error) = sink.map_buffer(buffer) {
            for prior in mapped.into_iter().rev() { let _ = sink.unmap_buffer(prior); }
            return Err(error);
        }
        mapped.push(buffer);
    }
    for resource in resources { ctx.graphics.get_mut(*resource).unwrap().mapped = true; }
    Ok(())
}

pub fn mapped_pointer(ctx: &CudaContext, resource: GraphicsResource) -> Result<(DevicePtr, u64)> {
    let entry = ctx.graphics.get(resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
    if !entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource is not mapped")); }
    Ok((entry.pointer, entry.bytes))
}

pub fn unmap_resources(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    resources: &[GraphicsResource],
) -> Result<()> {
    validate_distinct(resources)?;
    for resource in resources {
        let entry = ctx.graphics.get(*resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
        if !entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource is not mapped")); }
    }
    for resource in resources {
        sink.unmap_buffer(ctx.graphics.get(*resource).unwrap().buffer)?;
        ctx.graphics.get_mut(*resource).unwrap().mapped = false;
    }
    Ok(())
}

pub fn unregister_resource(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    resource: GraphicsResource,
) -> Result<()> {
    let entry = *ctx.graphics.get(resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
    if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource still mapped")); }
    sink.submit(&[Cmd::DestroyBuffer(entry.buffer.0)])?;
    ctx.graphics.remove(resource);
    ctx.mem.free(entry.pointer);
    Ok(())
}

fn validate_distinct(resources: &[GraphicsResource]) -> Result<()> {
    for (index, resource) in resources.iter().enumerate() {
        if resources[..index].contains(resource) {
            return Err(GpuError::Invalid("duplicate CUDA graphics resource"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CudaDeviceDesc;
    use hl_gpu::{Capabilities, FeatureRequest, FenceId};

    #[derive(Default)]
    struct Sink { imports: Vec<(BufferId, ExportId)>, mapped: Vec<BufferId>, unmapped: Vec<BufferId>, batches: Vec<Vec<Cmd>>, fail_map: Option<BufferId> }
    impl CommandSink for Sink {
        fn negotiate(&mut self, _: &FeatureRequest) -> Result<Capabilities> { Ok(Capabilities::permissive_fixture("graphics-test")) }
        fn submit(&mut self, batch: &[Cmd]) -> Result<()> { self.batches.push(batch.to_vec()); Ok(()) }
        fn wait(&mut self, _: FenceId, _: u64) -> Result<()> { Ok(()) }
        fn import_buffer(&mut self, id: BufferId, export: ExportId) -> Result<u64> { self.imports.push((id, export)); Ok(4096) }
        fn map_buffer(&mut self, id: BufferId) -> Result<()> { if self.fail_map == Some(id) { return Err(GpuError::Invalid("injected map failure")); } self.mapped.push(id); Ok(()) }
        fn unmap_buffer(&mut self, id: BufferId) -> Result<()> { self.unmapped.push(id); Ok(()) }
    }
    fn context() -> CudaContext { CudaContext::new(CudaDeviceDesc::apple_default(1 << 30)) }

    #[test]
    fn ordinary_buffer_lifecycle_exposes_pointer_only_while_mapped() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let resource = register_buffer(&mut ctx, &mut sink, ExportId(9)).unwrap();
        assert!(mapped_pointer(&ctx, resource).is_err());
        map_resources(&mut ctx, &mut sink, &[resource]).unwrap();
        let (pointer, bytes) = mapped_pointer(&ctx, resource).unwrap();
        assert_eq!(bytes, 4096);
        assert_eq!(ctx.resolve(pointer), Some((sink.imports[0].0, 0)));
        unmap_resources(&mut ctx, &mut sink, &[resource]).unwrap();
        assert!(mapped_pointer(&ctx, resource).is_err());
        unregister_resource(&mut ctx, &mut sink, resource).unwrap();
        assert!(ctx.resolve(pointer).is_none());
        assert!(matches!(sink.batches.last().unwrap().as_slice(), [Cmd::DestroyBuffer(_)]));
    }

    #[test]
    fn failed_multi_map_rolls_back_host_claims_and_local_state() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let a = register_buffer(&mut ctx, &mut sink, ExportId(1)).unwrap();
        let b = register_buffer(&mut ctx, &mut sink, ExportId(2)).unwrap();
        sink.fail_map = Some(ctx.graphics.get(b).unwrap().buffer);
        assert!(map_resources(&mut ctx, &mut sink, &[a, b]).is_err());
        assert_eq!(sink.unmapped, vec![ctx.graphics.get(a).unwrap().buffer]);
        assert!(!ctx.graphics.get(a).unwrap().mapped && !ctx.graphics.get(b).unwrap().mapped);
    }

    #[test]
    fn duplicate_and_mapped_unregister_are_refused_without_mutation() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let resource = register_buffer(&mut ctx, &mut sink, ExportId(1)).unwrap();
        assert!(map_resources(&mut ctx, &mut sink, &[resource, resource]).is_err());
        map_resources(&mut ctx, &mut sink, &[resource]).unwrap();
        assert!(unregister_resource(&mut ctx, &mut sink, resource).is_err());
        assert!(ctx.graphics.get(resource).is_some());
    }

    #[test]
    fn map_flags_validate_resource_value_and_mapping_state() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let resource = register_buffer(&mut ctx, &mut sink, ExportId(1)).unwrap();
        set_map_flags(&mut ctx, resource, 2).unwrap();
        assert_eq!(ctx.graphics.get(resource).unwrap().map_flags, 2);
        assert!(set_map_flags(&mut ctx, resource, 3).is_err());
        assert_eq!(ctx.graphics.get(resource).unwrap().map_flags, 2);
        map_resources(&mut ctx, &mut sink, &[resource]).unwrap();
        assert!(set_map_flags(&mut ctx, resource, 1).is_err());
        assert_eq!(ctx.graphics.get(resource).unwrap().map_flags, 2);
        assert!(set_map_flags(&mut ctx, GraphicsResource(u64::MAX), 0).is_err());
    }
}
