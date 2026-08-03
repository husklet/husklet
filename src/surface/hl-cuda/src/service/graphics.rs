//! CUDA graphics-resource lifecycle over hl-GPU's explicit export/import access gate.

use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::model::graphics::{GraphicsBuffer, GraphicsImage, GraphicsObject, GraphicsResource, ImportedArrayHandle};
use crate::model::stream::Stream;
use hl_gpu::{BufferId, Cmd, CommandSink, ExportId, GpuError, Result, TextureId};

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

/// Register the narrow image shape the GL bridge can truthfully export today: one `GL_TEXTURE_2D`
/// image. Mip/layer selection is validated later by [`mapped_array`].
pub fn register_image(ctx: &mut CudaContext, sink: &mut dyn CommandSink, export: ExportId, flags: u32) -> Result<GraphicsResource> {
    if !matches!(flags, 0 | 1 | 2 | 4 | 8) {
        return Err(GpuError::Invalid("CUDA graphics image registration flags"));
    }
    let texture = TextureId(ctx.alloc_texture());
    sink.import_texture(texture, export)?;
    Ok(ctx.graphics.insert_image(GraphicsImage { texture, export, mapped: false, map_flags: 0, registration_flags: flags, array: None }))
}

/// Set NONE, READ_ONLY, or WRITE_DISCARD for a registered, currently-unmapped resource.
pub fn set_map_flags(ctx: &mut CudaContext, resource: GraphicsResource, flags: u32) -> Result<()> {
    if flags > 2 { return Err(GpuError::Invalid("CUDA graphics map flags")); }
    let entry = ctx.graphics.object_mut(resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
    match entry {
        GraphicsObject::Buffer(entry) => { if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource still mapped")); } entry.map_flags = flags; }
        GraphicsObject::Image(entry) => { if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource still mapped")); } entry.map_flags = flags; }
    }
    Ok(())
}

pub fn map_resources(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    resources: &[GraphicsResource],
    stream: Stream,
) -> Result<()> {
    validate_stream(ctx, stream)?;
    validate_distinct(resources)?;
    for resource in resources {
        let entry = ctx.graphics.object(*resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
        if match entry { GraphicsObject::Buffer(entry) => entry.mapped, GraphicsObject::Image(entry) => entry.mapped } { return Err(GpuError::Invalid("CUDA graphics resource already mapped")); }
    }
    let mut mapped = Vec::new();
    for resource in resources {
        let object = *ctx.graphics.object(*resource).unwrap();
        let result = match object { GraphicsObject::Buffer(entry) => sink.map_buffer(entry.buffer), GraphicsObject::Image(entry) => sink.map_texture(entry.texture) };
        if let Err(error) = result {
            for prior in mapped.into_iter().rev() { match prior { GraphicsObject::Buffer(entry) => { let _ = sink.unmap_buffer(entry.buffer); }, GraphicsObject::Image(entry) => { let _ = sink.unmap_texture(entry.texture); } } }
            return Err(error);
        }
        mapped.push(object);
    }
    for resource in resources { match ctx.graphics.object_mut(*resource).unwrap() { GraphicsObject::Buffer(entry) => entry.mapped = true, GraphicsObject::Image(entry) => entry.mapped = true } }
    Ok(())
}

pub fn mapped_array(ctx: &mut CudaContext, resource: GraphicsResource, array_index: u32, mip_level: u32) -> Result<ImportedArrayHandle> {
    ctx.graphics.mapped_array(resource, mip_level, array_index).ok_or(GpuError::Invalid("CUDA mapped image subresource"))
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
    stream: Stream,
) -> Result<()> {
    validate_stream(ctx, stream)?;
    validate_distinct(resources)?;
    for resource in resources {
        let entry = ctx.graphics.object(*resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
        if !match entry { GraphicsObject::Buffer(entry) => entry.mapped, GraphicsObject::Image(entry) => entry.mapped } { return Err(GpuError::Invalid("CUDA graphics resource is not mapped")); }
    }
    for resource in resources {
        let object = *ctx.graphics.object(*resource).unwrap();
        match object { GraphicsObject::Buffer(entry) => sink.unmap_buffer(entry.buffer)?, GraphicsObject::Image(entry) => sink.unmap_texture(entry.texture)? }
        ctx.graphics.invalidate_array(*resource);
        match ctx.graphics.object_mut(*resource).unwrap() { GraphicsObject::Buffer(entry) => entry.mapped = false, GraphicsObject::Image(entry) => entry.mapped = false }
    }
    Ok(())
}

pub fn unregister_resource(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    resource: GraphicsResource,
) -> Result<()> {
    let entry = *ctx.graphics.object(resource).ok_or(GpuError::Invalid("CUDA graphics resource"))?;
    match entry {
        GraphicsObject::Buffer(entry) => {
            if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource still mapped")); }
            sink.submit(&[Cmd::DestroyBuffer(entry.buffer.0)])?;
            ctx.graphics.remove_object(resource);
            ctx.mem.free(entry.pointer);
        }
        GraphicsObject::Image(entry) => {
            if entry.mapped { return Err(GpuError::Invalid("CUDA graphics resource still mapped")); }
            sink.submit(&[Cmd::DestroyTexture(entry.texture.0)])?;
            ctx.graphics.remove_object(resource);
        }
    }
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

fn validate_stream(ctx: &CudaContext, stream: Stream) -> Result<()> {
    ctx.streams.is_valid(stream).then_some(()).ok_or(GpuError::Invalid("CUDA graphics resource stream"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CudaDeviceDesc;
    use hl_gpu::{Capabilities, FeatureRequest, FenceId};

    #[derive(Default)]
    struct Sink { imports: Vec<(BufferId, ExportId)>, texture_imports: Vec<(TextureId, ExportId)>, mapped: Vec<BufferId>, unmapped: Vec<BufferId>, mapped_textures: Vec<TextureId>, unmapped_textures: Vec<TextureId>, batches: Vec<Vec<Cmd>>, fail_map: Option<BufferId> }
    impl CommandSink for Sink {
        fn negotiate(&mut self, _: &FeatureRequest) -> Result<Capabilities> { Ok(Capabilities::permissive_fixture("graphics-test")) }
        fn submit(&mut self, batch: &[Cmd]) -> Result<()> { self.batches.push(batch.to_vec()); Ok(()) }
        fn wait(&mut self, _: FenceId, _: u64) -> Result<()> { Ok(()) }
        fn import_buffer(&mut self, id: BufferId, export: ExportId) -> Result<u64> { self.imports.push((id, export)); Ok(4096) }
        fn import_texture(&mut self, id: TextureId, export: ExportId) -> Result<u64> { self.texture_imports.push((id, export)); Ok(64) }
        fn map_buffer(&mut self, id: BufferId) -> Result<()> { if self.fail_map == Some(id) { return Err(GpuError::Invalid("injected map failure")); } self.mapped.push(id); Ok(()) }
        fn unmap_buffer(&mut self, id: BufferId) -> Result<()> { self.unmapped.push(id); Ok(()) }
        fn map_texture(&mut self, id: TextureId) -> Result<()> { self.mapped_textures.push(id); Ok(()) }
        fn unmap_texture(&mut self, id: TextureId) -> Result<()> { self.unmapped_textures.push(id); Ok(()) }
    }
    fn context() -> CudaContext { CudaContext::new(CudaDeviceDesc::apple_default(1 << 30)) }

    #[test]
    fn ordinary_buffer_lifecycle_exposes_pointer_only_while_mapped() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let resource = register_buffer(&mut ctx, &mut sink, ExportId(9)).unwrap();
        assert!(mapped_pointer(&ctx, resource).is_err());
        map_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
        let (pointer, bytes) = mapped_pointer(&ctx, resource).unwrap();
        assert_eq!(bytes, 4096);
        assert_eq!(ctx.resolve(pointer), Some((sink.imports[0].0, 0)));
        unmap_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
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
        assert!(map_resources(&mut ctx, &mut sink, &[a, b], Stream(0)).is_err());
        assert_eq!(sink.unmapped, vec![ctx.graphics.get(a).unwrap().buffer]);
        assert!(!ctx.graphics.get(a).unwrap().mapped && !ctx.graphics.get(b).unwrap().mapped);
    }

    #[test]
    fn duplicate_and_mapped_unregister_are_refused_without_mutation() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let resource = register_buffer(&mut ctx, &mut sink, ExportId(1)).unwrap();
        assert!(map_resources(&mut ctx, &mut sink, &[resource, resource], Stream(0)).is_err());
        map_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
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
        map_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
        assert!(set_map_flags(&mut ctx, resource, 1).is_err());
        assert_eq!(ctx.graphics.get(resource).unwrap().map_flags, 2);
        assert!(set_map_flags(&mut ctx, GraphicsResource(u64::MAX), 0).is_err());
    }

    #[test]
    fn imported_image_array_exists_only_for_the_mapped_base_subresource() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        assert!(register_image(&mut ctx, &mut sink, ExportId(7), 3).is_err());
        let resource = register_image(&mut ctx, &mut sink, ExportId(7), 4).unwrap();
        assert!(matches!(ctx.graphics.object(resource), Some(GraphicsObject::Image(image)) if image.registration_flags == 4));
        assert_eq!(sink.texture_imports.len(), 1);
        assert!(mapped_array(&mut ctx, resource, 0, 0).is_err());
        map_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
        assert!(mapped_pointer(&ctx, resource).is_err());
        let array = mapped_array(&mut ctx, resource, 0, 0).unwrap();
        assert_eq!(mapped_array(&mut ctx, resource, 0, 0).unwrap(), array);
        assert!(mapped_array(&mut ctx, resource, 1, 0).is_err());
        assert!(mapped_array(&mut ctx, resource, 0, 1).is_err());
        let imported = *ctx.graphics.array(array).unwrap();
        assert_eq!((imported.resource, imported.mip, imported.layer), (resource, 0, 0));
        assert_eq!(sink.mapped_textures, vec![imported.texture]);
        assert!(unregister_resource(&mut ctx, &mut sink, resource).is_err());
        unmap_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
        assert!(ctx.graphics.array(array).is_none());
        assert!(mapped_array(&mut ctx, resource, 0, 0).is_err());
        unregister_resource(&mut ctx, &mut sink, resource).unwrap();
        assert_eq!(sink.unmapped_textures, vec![imported.texture]);
        assert!(matches!(sink.batches.last().unwrap().as_slice(), [Cmd::DestroyTexture(_)]));
    }

    #[test]
    fn map_and_unmap_reject_unknown_stream_before_touching_the_resource() {
        let (mut ctx, mut sink) = (context(), Sink::default());
        let resource = register_buffer(&mut ctx, &mut sink, ExportId(1)).unwrap();
        assert!(map_resources(&mut ctx, &mut sink, &[resource], Stream(u32::MAX)).is_err());
        assert!(sink.mapped.is_empty());
        assert!(!ctx.graphics.get(resource).unwrap().mapped);
        map_resources(&mut ctx, &mut sink, &[resource], Stream(0)).unwrap();
        assert!(unmap_resources(&mut ctx, &mut sink, &[resource], Stream(u32::MAX)).is_err());
        assert!(sink.unmapped.is_empty());
        assert!(ctx.graphics.get(resource).unwrap().mapped);
    }
}
