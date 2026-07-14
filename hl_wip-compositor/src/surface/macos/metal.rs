//! The Metal side of the macOS presenter: the shared `MTLDevice` + command queue, BGRA texture
//! upload/wrap/readback, and the composite render pass that samples a surface texture over the window
//! background. Ported from `hl-display::metal` + the composite pipeline in `hl-display::present_cocoa`.

use objc2::msg_send_id;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRegion,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage,
};

use super::iosurface::IOSurfaceRef;

#[link(name = "Metal", kind = "framework")]
extern "C" {
    // objc2-metal deliberately does not bind this free function (it needs the framework link).
    fn MTLCreateSystemDefaultDevice() -> *mut ProtocolObject<dyn MTLDevice>;
}

/// The shared host Metal context: one system `MTLDevice` + one command queue. The same device the
/// `hl-gpu` executor would target, so a guest's rendered `MTLTexture`/IOSurface composites with no
/// cross-device copy.
pub struct MetalCtx {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

impl MetalCtx {
    /// Create from the system default GPU. `None` if Metal is unavailable (no GPU / not a macOS GPU).
    pub fn new() -> Option<MetalCtx> {
        let device = unsafe { MTLCreateSystemDefaultDevice() };
        if device.is_null() {
            return None;
        }
        // MTLCreateSystemDefaultDevice returns a +1 retained device; adopt it as a Retained.
        let device = unsafe { Retained::from_raw(device)? };
        let queue = device.newCommandQueue()?;
        Some(MetalCtx { device, queue })
    }

    /// The adapter/device name (e.g. "Apple M-series"), for logging which GPU the present ran on.
    pub fn device_name(&self) -> String {
        self.device.name().to_string()
    }

    /// A `BGRA8Unorm` texture usable as a render/blit target and readable back on the CPU
    /// (`storageModeShared` — unified memory on Apple Silicon, so the readback is free).
    pub fn new_bgra_texture(&self, w: u32, h: u32) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                w.max(1) as usize,
                h.max(1) as usize,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Shared);
        self.device
            .newTextureWithDescriptor(&desc)
            .expect("newTextureWithDescriptor failed")
    }

    /// Upload tight BGRA bytes into a fresh GPU texture (real `replaceRegion:` upload).
    pub fn upload_bgra(
        &self,
        bgra: &[u8],
        w: u32,
        h: u32,
    ) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let tex = self.new_bgra_texture(w, h);
        let region = full_region(w, h);
        unsafe {
            tex.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                std::ptr::NonNull::new(bgra.as_ptr() as *mut std::ffi::c_void).unwrap(),
                (w * 4) as usize,
            );
        }
        tex
    }

    /// Wrap a host `IOSurface` as an `MTLTexture` with **zero copy** — the IOSurface's pages ARE the
    /// texture's storage (the GPU-rung-2 mechanism a guest's IOSurface-backed dmabuf uses).
    pub fn texture_from_iosurface(
        &self,
        surface: IOSurfaceRef,
        w: u32,
        h: u32,
    ) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                w.max(1) as usize,
                h.max(1) as usize,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Shared);
        // newTextureWithDescriptor:iosurface:plane: — not bound by objc2-metal 0.2, so raw-send it.
        unsafe {
            msg_send_id![&*self.device, newTextureWithDescriptor: &*desc, iosurface: surface, plane: 0usize]
        }
    }

    /// Read a `BGRA8Unorm` texture back to CPU bytes (BGRA, tight `w*4` rows).
    pub fn readback_bgra(&self, tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        let region = full_region(w, h);
        unsafe {
            tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(out.as_mut_ptr() as *mut _).unwrap(),
                (w * 4) as usize,
                region,
                0,
            );
        }
        out
    }

    /// Build the composite render pipeline: a full-screen triangle that samples the surface texture and
    /// blends it over an opaque white window background (`min(c.rgb + bg*(1-a), 1)`) so an ARGB surface
    /// shows the compositor background instead of black margins. `uv_rect = (u0,v0,u1,v1)` crops the
    /// backing texture into the on-screen region. Identical MSL to the old `hl-display` present path.
    pub fn make_composite_pipeline(
        &self,
    ) -> Option<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> {
        const SRC: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VOut {
    float4 pos [[position]];
    float2 uv;
};

vertex VOut vmain(uint vid [[vertex_id]]) {
    float2 pos[3] = { float2(-1.0, -1.0), float2( 3.0, -1.0), float2(-1.0,  3.0) };
    float2 uv[3]  = { float2( 0.0,  1.0), float2( 2.0,  1.0), float2( 0.0, -1.0) };
    VOut out;
    out.pos = float4(pos[vid], 0.0, 1.0);
    out.uv = uv[vid];
    return out;
}

fragment float4 fmain(VOut in [[stage_in]], texture2d<float> src [[texture(0)]],
                      constant float4& uv_rect [[buffer(0)]]) {
    constexpr sampler smp(address::clamp_to_edge, filter::nearest);
    float2 uv = float2(mix(uv_rect.x, uv_rect.z, in.uv.x),
                       mix(uv_rect.y, uv_rect.w, in.uv.y));
    float4 c = src.sample(smp, uv);
    float3 bg = float3(1.0, 1.0, 1.0);
    return float4(min(c.rgb + bg * (1.0 - c.a), 1.0), 1.0);
}
"#;
        let lib = match self
            .device
            .newLibraryWithSource_options_error(&NSString::from_str(SRC), None)
        {
            Ok(lib) => lib,
            Err(err) => {
                eprintln!("[macos-surface] composite MSL compile failed: {err:?}");
                return None;
            }
        };
        let vfn = lib.newFunctionWithName(&NSString::from_str("vmain"))?;
        let ffn = lib.newFunctionWithName(&NSString::from_str("fmain"))?;
        let pdesc = MTLRenderPipelineDescriptor::new();
        pdesc.setVertexFunction(Some(&vfn));
        pdesc.setFragmentFunction(Some(&ffn));
        unsafe {
            pdesc
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        }
        match self.device.newRenderPipelineStateWithDescriptor_error(&pdesc) {
            Ok(pipeline) => Some(pipeline),
            Err(err) => {
                eprintln!("[macos-surface] composite pipeline creation failed: {err:?}");
                None
            }
        }
    }

    /// Composite `src` (the surface texture) into `dst` (the offscreen target or a drawable texture) with
    /// the composite pipeline, sampling the `uv_rect` sub-region over the white background. Synchronous.
    #[allow(clippy::too_many_arguments)]
    pub fn compose_into(
        &self,
        pipeline: &ProtocolObject<dyn MTLRenderPipelineState>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
        uv_rect: [f32; 4],
    ) {
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let ca = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(dst));
        ca.setLoadAction(MTLLoadAction::Clear);
        ca.setClearColor(MTLClearColor {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        });
        ca.setStoreAction(MTLStoreAction::Store);
        let cmd = self.queue.commandBuffer().expect("commandBuffer");
        let enc = cmd
            .renderCommandEncoderWithDescriptor(&pass)
            .expect("render encoder");
        enc.setRenderPipelineState(pipeline);
        unsafe {
            enc.setFragmentTexture_atIndex(Some(src), 0);
            let uv = std::ptr::NonNull::new(uv_rect.as_ptr() as *mut std::ffi::c_void)
                .expect("uv rect pointer");
            enc.setFragmentBytes_length_atIndex(uv, std::mem::size_of_val(&uv_rect), 0);
            enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        enc.endEncoding();
        cmd.commit();
        unsafe { cmd.waitUntilCompleted() };
    }

    /// GPU blit `src` → `dst` (same size), synchronously. Used to copy a composited texture into a
    /// `CAMetalLayer` drawable.
    pub fn blit(&self, src: &ProtocolObject<dyn MTLTexture>, dst: &ProtocolObject<dyn MTLTexture>) {
        let cmd = self.queue.commandBuffer().expect("commandBuffer");
        let enc = cmd.blitCommandEncoder().expect("blitCommandEncoder");
        unsafe { enc.copyFromTexture_toTexture(src, dst) };
        enc.endEncoding();
        cmd.commit();
        unsafe { cmd.waitUntilCompleted() };
    }
}

/// Convert tight BGRA bytes to RGBA (swap B/R; force opaque alpha) for PNG encoding / pixel inspection.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = vec![0u8; bgra.len()];
    for i in (0..bgra.len()).step_by(4) {
        rgba[i] = bgra[i + 2];
        rgba[i + 1] = bgra[i + 1];
        rgba[i + 2] = bgra[i];
        rgba[i + 3] = 0xff;
    }
    rgba
}

fn full_region(w: u32, h: u32) -> MTLRegion {
    MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize {
            width: w as usize,
            height: h as usize,
            depth: 1,
        },
    }
}
