//! The Metal side of the macOS presenter: the shared `MTLDevice` + command queue, BGRA texture
//! upload/wrap/readback, and the composite render pass that samples a surface texture over the window
//! background. Ported from `hl-display::metal` + the composite pipeline in `hl-display::present_cocoa`.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRegion,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLScissorRect, MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage, MTLViewport,
};

type IOSurfaceRef = *mut std::ffi::c_void;

#[link(name = "Metal", kind = "framework")]
extern "C" {
    // objc2-metal deliberately does not bind this free function (it needs the framework link).
    fn MTLCreateSystemDefaultDevice() -> *mut ProtocolObject<dyn MTLDevice>;
}

#[link(name = "objc")]
extern "C" {
    fn objc_msgSend();
    fn sel_registerName(name: *const std::ffi::c_char) -> *const std::ffi::c_void;
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

    /// Wait until all Metal work submitted before this call has completed.
    ///
    /// Normal presentation remains asynchronous. Diagnostic capture uses this one-shot queue barrier
    /// when a request arrives after the last frame was already submitted, so CPU texture reads cannot
    /// race the render that produced that frame.
    pub fn wait_idle(&self) {
        let command = self.queue.commandBuffer().expect("commandBuffer");
        command.commit();
        // SAFETY: `command` is retained for this call and was committed to this context's live queue.
        unsafe { command.waitUntilCompleted() };
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
        let region = FrameSize::new(w, h).region();
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

    /// Replace all pixels in an existing shared BGRA texture. The native presenter retains this texture
    /// across same-sized wl_shm commits so animation does not allocate and retire a multi-megabyte Metal
    /// resource every frame.
    pub fn update_bgra(&self, tex: &ProtocolObject<dyn MTLTexture>, bgra: &[u8], w: u32, h: u32) {
        let region = FrameSize::new(w, h).region();
        unsafe {
            tex.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                std::ptr::NonNull::new(bgra.as_ptr() as *mut std::ffi::c_void).unwrap(),
                (w * 4) as usize,
            );
        }
    }

    /// Wrap a host `IOSurface` as an `MTLTexture` with **zero copy** — the IOSurface's pages ARE the
    /// texture's storage (the GPU-rung-2 mechanism a guest's IOSurface-backed dmabuf uses).
    pub fn texture_from_iosurface(
        &self,
        surface: &hl_iosurface::Surface,
        w: u32,
        h: u32,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, &'static str> {
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
        type NewTexture = unsafe extern "C" fn(
            *const ProtocolObject<dyn MTLDevice>,
            *const std::ffi::c_void,
            *const MTLTextureDescriptor,
            IOSurfaceRef,
            usize,
        ) -> *mut ProtocolObject<dyn MTLTexture>;
        let selector =
            unsafe { sel_registerName(c"newTextureWithDescriptor:iosurface:plane:".as_ptr()) };
        let call: NewTexture = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        let raw = unsafe {
            call(
                Retained::as_ptr(&self.device),
                selector,
                Retained::as_ptr(&desc),
                surface.handle().as_ptr(),
                0,
            )
        };
        unsafe { Retained::from_raw(raw) }.ok_or("Metal refused IOSurface texture")
    }

    /// Read a `BGRA8Unorm` texture back to CPU bytes (BGRA, tight `w*4` rows).
    pub fn readback_bgra(&self, tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        let region = FrameSize::new(w, h).region();
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
    /// preserves the surface color and alpha. `uv_map` maps normalized destination coordinates into the
    /// backing texture, composing Wayland buffer transforms and crops exactly once.
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
                      constant float4* uv_map [[buffer(0)]],
                      constant uint& force_opaque [[buffer(1)]]) {
    constexpr sampler smp(address::clamp_to_edge, filter::nearest);
    float2 uv = uv_map[0].xy + in.uv.x * uv_map[0].zw + in.uv.y * uv_map[1].xy;
    float4 c = src.sample(smp, uv);
    return float4(c.rgb, force_opaque ? 1.0 : c.a);
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
        match self
            .device
            .newRenderPipelineStateWithDescriptor_error(&pdesc)
        {
            Ok(pipeline) => Some(pipeline),
            Err(err) => {
                eprintln!("[macos-surface] composite pipeline creation failed: {err:?}");
                None
            }
        }
    }

    /// Composite `src` (the surface texture) into `dst` (the offscreen target or a drawable texture) with
    /// the composite pipeline, sampling through an affine UV map and optionally forcing XRGB opacity.
    #[allow(clippy::too_many_arguments)]
    pub fn compose_into(
        &self,
        pipeline: &ProtocolObject<dyn MTLRenderPipelineState>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
        uv_map: [f32; 8],
        force_opaque: bool,
        damage: Option<&[crate::scene::model::Rect]>,
        viewport: Option<crate::scene::model::Rect>,
        clear: bool,
        synchronize: bool,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, &'static str> {
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let ca = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(dst));
        ca.setLoadAction(if !clear && (damage.is_some() || viewport.is_some()) {
            MTLLoadAction::Load
        } else {
            MTLLoadAction::Clear
        });
        ca.setClearColor(MTLClearColor {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 0.0,
        });
        ca.setStoreAction(MTLStoreAction::Store);
        let cmd = self.queue.commandBuffer().ok_or("missing command buffer")?;
        let enc = cmd
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or("missing render encoder")?;
        enc.setRenderPipelineState(pipeline);
        if let Some(rect) = viewport {
            enc.setViewport(MTLViewport {
                originX: f64::from(rect.x),
                originY: f64::from(rect.y),
                width: f64::from(rect.w.max(0)),
                height: f64::from(rect.h.max(0)),
                znear: 0.0,
                zfar: 1.0,
            });
        }
        unsafe {
            enc.setFragmentTexture_atIndex(Some(src), 0);
            let uv = std::ptr::NonNull::new(uv_map.as_ptr() as *mut std::ffi::c_void)
                .expect("uv map pointer");
            enc.setFragmentBytes_length_atIndex(uv, std::mem::size_of_val(&uv_map), 0);
            let opaque = u32::from(force_opaque);
            let opaque_ptr = std::ptr::NonNull::new(
                (&opaque as *const u32)
                    .cast_mut()
                    .cast::<std::ffi::c_void>(),
            )
            .expect("opaque flag pointer");
            enc.setFragmentBytes_length_atIndex(opaque_ptr, std::mem::size_of_val(&opaque), 1);
            if let Some(regions) = damage {
                for rect in regions {
                    enc.setScissorRect(MTLScissorRect {
                        x: rect.x as usize,
                        y: rect.y as usize,
                        width: rect.w as usize,
                        height: rect.h as usize,
                    });
                    enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
                }
            } else {
                enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
            }
        }
        enc.endEncoding();
        cmd.commit();
        if synchronize {
            unsafe { cmd.waitUntilCompleted() };
        }
        Ok(cmd)
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

/// Convert tight BGRA bytes to RGBA (swap B/R and preserve alpha) for capture / pixel inspection.
pub struct BgraFrame<'a> {
    pixels: &'a [u8],
}

impl<'a> BgraFrame<'a> {
    pub fn new(pixels: &'a [u8]) -> Self {
        Self { pixels }
    }

    pub fn rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.pixels.len() / 4 * 4);
        for pixel in self.pixels.chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        rgba
    }
}

struct FrameSize {
    width: u32,
    height: u32,
}

impl FrameSize {
    fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    fn region(self) -> MTLRegion {
        MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: self.width as usize,
                height: self.height as usize,
                depth: 1,
            },
        }
    }
}
