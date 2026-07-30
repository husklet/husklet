use super::*;
use hl_gpu::protocol::model::capability::{Capabilities, FeatureRequest};
use hl_gpu::{BufferId, CommandSink, FenceId, GpuError, Result};

struct RejectOnce {
    rejected: Option<Vec<Cmd>>,
    accepted: RecordingSink,
}

struct FailRead {
    accepted: RecordingSink,
}

impl CommandSink for FailRead {
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities> {
        self.accepted.negotiate(request)
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        self.accepted.submit(batch)
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        self.accepted.wait(fence, value)
    }

    fn read_buffer(&mut self, _id: BufferId, _offset: u64, _len: usize) -> Result<Vec<u8>> {
        Err(GpuError::Decode("expected read failure".into()))
    }
}

impl RejectOnce {
    fn new() -> Self {
        Self {
            rejected: None,
            accepted: RecordingSink::with_full_caps(),
        }
    }
}

impl CommandSink for RejectOnce {
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities> {
        self.accepted.negotiate(request)
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        if self.rejected.is_none() {
            self.rejected = Some(batch.to_vec());
            Err(GpuError::ResourceLimit("reject-once"))
        } else {
            self.accepted.submit(batch)
        }
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        self.accepted.wait(fence, value)
    }

    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.accepted.read_buffer(id, offset, len)
    }
}

#[test]
fn rejected_frame_retries_with_complete_identical_resource_creates() {
    let mut context = ctx_64();
    let mut sink = RejectOnce::new();
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert_eq!(
        swap::swap_buffers(&mut context, &mut sink),
        Err(GpuError::ResourceLimit("reject-once"))
    );
    assert_eq!(context.draws().len(), 1, "rejected draw must remain queued");

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(context.draws().is_empty());

    let rejected = sink.rejected.expect("rejected command batch");
    let accepted = sink
        .accepted
        .batches
        .first()
        .expect("accepted retry command batch");
    assert_eq!(
        accepted, &rejected,
        "retry must recreate the exact resources the rejected transaction rolled back"
    );
    assert!(accepted
        .iter()
        .any(|command| matches!(command, Cmd::CreateRenderPipeline(..))));
    assert!(accepted
        .iter()
        .any(|command| matches!(command, Cmd::CreateBuffer(..))));
    assert!(accepted
        .iter()
        .any(|command| matches!(command, Cmd::CreateTexture(..))));
}

#[test]
fn rejected_readback_retries_semantic_frame_then_retires_temporary_buffer() {
    let mut context = ctx_64();
    let mut sink = RejectOnce::new();
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert_eq!(
        hl_gl::service::readpixels::read_pixels(&mut context, &mut sink, 0, 0, 1, 1, GL_RGBA,),
        Err(GpuError::ResourceLimit("reject-once"))
    );
    assert_eq!(context.draws().len(), 1);

    let pixels =
        hl_gl::service::readpixels::read_pixels(&mut context, &mut sink, 0, 0, 1, 1, GL_RGBA)
            .expect("retry");
    assert_eq!(pixels.len(), 4);
    assert!(context.draws().is_empty());
    let rejected = sink.rejected.expect("rejected readback frame");
    assert_eq!(sink.accepted.batches[0].len(), rejected.len());
    assert!(matches!(
        sink.accepted.batches[0].last(),
        Some(Cmd::Submit(_))
    ));
    assert!(matches!(
        sink.accepted.batches[1].as_slice(),
        [Cmd::DestroyBuffer(_)]
    ));
}

#[test]
fn accepted_frame_is_not_replayed_when_separate_read_target_is_absent() {
    let mut context = ctx_64();
    let draw = context.surface();
    context.bind_surfaces(
        11,
        draw,
        hl_gl::model::context::SurfaceKind::Window,
        12,
        GlSurface {
            have: true,
            width: 32,
            height: 32,
        },
        hl_gl::model::context::SurfaceKind::Offscreen,
    );
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let pixels =
        hl_gl::service::readpixels::read_pixels(&mut context, &mut sink, 0, 0, 1, 1, GL_RGBA)
            .expect("separate read");

    assert_eq!(pixels, [0; 4]);
    assert!(context.draws().is_empty());
    assert_eq!(sink.batches.len(), 1);
    assert!(!swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert_eq!(sink.batches.len(), 1, "accepted draw must not replay");
}

#[test]
fn read_failure_after_submit_keeps_frame_committed_and_retires_buffer() {
    let mut context = ctx_64();
    let mut sink = FailRead {
        accepted: RecordingSink::with_full_caps(),
    };
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let error =
        hl_gl::service::readpixels::read_pixels(&mut context, &mut sink, 0, 0, 1, 1, GL_RGBA)
            .expect_err("read failure");

    assert!(error.to_string().contains("expected read failure"));
    assert!(context.draws().is_empty());
    assert!(context.pending_destroys().is_empty());
    assert!(matches!(
        sink.accepted.batches[1].as_slice(),
        [Cmd::DestroyBuffer(_)]
    ));
}

#[test]
fn rejected_shared_image_upload_retries_the_complete_transaction() {
    let mut context = ctx_64();
    let mut sink = RejectOnce::new();
    super::texture::program(&mut context);
    tri_vbo(&mut context, 8);
    let (texture, _storage) =
        super::texture::shared_texture(&mut context, std::sync::Arc::new(vec![89; 4 * 4 * 4]));
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(texture));

    assert_eq!(
        swap::swap_buffers(&mut context, &mut sink),
        Err(GpuError::ResourceLimit("reject-once"))
    );
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert_eq!(
        sink.accepted.batches[0],
        sink.rejected.expect("rejected shared upload")
    );
    assert!(sink.accepted.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::CreateSampler(..))));
}

#[test]
fn rejected_capture_cleanup_retries_the_same_buffer_retirement() {
    let mut context = ctx_64();
    let mut sink = RejectOnce::new();
    context.queue_buffer_destroy(41);

    assert_eq!(
        context.flush_retirements(&mut sink),
        Err(GpuError::ResourceLimit("reject-once"))
    );
    assert_eq!(context.pending_destroys(), &[Cmd::DestroyBuffer(41)]);
    assert_eq!(context.flush_retirements(&mut sink).unwrap(), 1);
    assert!(context.pending_destroys().is_empty());
    assert_eq!(
        sink.accepted.batches[0],
        sink.rejected.expect("rejected cleanup batch")
    );
}

#[test]
fn rejected_mixed_cleanup_keeps_ready_and_pinned_retirements_transactional() {
    let mut context = ctx_64();
    let mut resident = RecordingSink::with_full_caps();
    super::texture::program(&mut context);
    tri_vbo(&mut context, 8);
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    record::tex_image_2d(&mut context, 2, 2, &[0x7c; 2 * 2 * 4]);
    record::uniform_sampler(&mut context, 0, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut resident).unwrap());
    let texture_ir = resident.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor)
                if descriptor.width == 2 && descriptor.height == 2 =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("resident sampled texture");

    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(texture));
    context.queue_buffer_destroy(41);
    let original = context.pending_destroys().to_vec();
    assert!(original.contains(&Cmd::DestroyTexture(texture_ir)));
    assert!(original.contains(&Cmd::DestroyBuffer(41)));

    let mut sink = RejectOnce::new();
    assert_eq!(
        context.flush_retirements(&mut sink),
        Err(GpuError::ResourceLimit("reject-once"))
    );
    assert_eq!(
        context.pending_destroys(),
        original,
        "a rejected ready subset cannot mutate the retirement queue"
    );

    assert_eq!(context.flush_retirements(&mut sink).unwrap(), 1);
    assert_eq!(sink.accepted.batches[0], vec![Cmd::DestroyBuffer(41)]);
    assert_eq!(
        context.pending_destroys(),
        &[Cmd::DestroyTexture(texture_ir)],
        "only the texture pinned by deferred work remains"
    );
}

#[test]
fn deferred_draw_relowers_deleted_buffer_after_standalone_retirement() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);
    let buffer = tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let old = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.usage == hl_gpu::protocol::model::enums::buffer_usage::VERTEX =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("resident vertex buffer");

    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_buffer(buffer));
    assert_eq!(context.flush_retirements(&mut sink).unwrap(), 1);
    assert_eq!(sink.batches[1], vec![Cmd::DestroyBuffer(old)]);

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let new = sink.batches[2]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.usage == hl_gpu::protocol::model::enums::buffer_usage::VERTEX =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("deferred draw re-uploads its immutable buffer snapshot");
    assert_ne!(new, old);
    assert!(sink.batches[2].iter().any(|command| matches!(
        command,
        Cmd::Submit(command_buffer)
            if command_buffer.encoder.iter().any(|operation| matches!(
                operation,
                hl_gpu::protocol::model::command::Enc::SetVertexBuffer { buffer, .. }
                    if *buffer == new
            ))
    )));
}

#[test]
fn rejected_framebuffer_write_does_not_claim_texture_authority() {
    let mut context = ctx_64();
    let mut sink = RejectOnce::new();
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    record::tex_image_2d_format(
        &mut context,
        8,
        8,
        &[0x44; 8 * 8 * 4],
        TextureFormat::R8Unorm,
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert_eq!(
        swap::flush(&mut context, &mut sink),
        Err(GpuError::ResourceLimit("reject-once"))
    );
    assert!(
        !context
            .textures
            .get(texture)
            .expect("texture")
            .gpu_authoritative(),
        "building or rejecting a render batch cannot supersede the CPU upload"
    );

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    assert!(
        context
            .textures
            .get(texture)
            .expect("texture")
            .gpu_authoritative(),
        "only the accepted retry publishes the framebuffer write"
    );
}
