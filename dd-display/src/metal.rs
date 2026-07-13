//! The Metal present path (macOS) — the hardware-acceleration foundation for the display pipeline AND
//! the shared host `MTLDevice`/`MTLCommandQueue` the `dd-gpu` executor targets.
//!
//! [`MetalCtx`] owns the one system `MTLDevice` + a command queue. The display side uploads a committed
//! `wl_shm` buffer as a `BGRA8Unorm` `MTLTexture` (`replaceRegion:` — a real GPU upload; the zero-copy
//! `MTLBuffer(bytesNoCopy)` over the pool pages is the next rung) and blits it to the target (an offscreen
//! texture for the readback proof, or a `CAMetalLayer` drawable for the live window).
//!
//! **`dd-gpu` seam:** [`MetalCtx`] is `pub` and constructible from an existing device
//! ([`MetalCtx::from_device`]), so `dd-gpu`'s `MetalBackend` (the CUDA/GPU-forwarding executor) and this
//! display path share ONE `MTLDevice` + queue — a guest's rendered `MTLTexture`/IOSurface can then be
//! composited by `dd-display` with no cross-device copy (GPU rung 2).
//!
//! Compiled only on macOS.

#![cfg(target_os = "macos")]

use objc2::msg_send_id;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLDevice, MTLEvent, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType,
    MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use std::ffi::c_void;

fn display_debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("DD_DISPLAY_DEBUG").is_some())
}

#[link(name = "Metal", kind = "framework")]
extern "C" {
    // objc2-metal deliberately does not bind this free function (it needs the framework link); declare it.
    fn MTLCreateSystemDefaultDevice() -> *mut ProtocolObject<dyn MTLDevice>;
}

// ---- IOSurface FFI (objc2 has no IOSurface binding; declare the C API + link the framework) ----
pub type IOSurfaceRef = *mut c_void;
type CFStringRef = *const c_void;

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    static kIOSurfaceWidth: CFStringRef;
    static kIOSurfaceHeight: CFStringRef;
    static kIOSurfaceBytesPerElement: CFStringRef;
    static kIOSurfaceBytesPerRow: CFStringRef;
    static kIOSurfacePixelFormat: CFStringRef;
    fn IOSurfaceCreate(properties: *const c_void) -> IOSurfaceRef;
    fn IOSurfaceLock(s: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceUnlock(s: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceGetBaseAddress(s: IOSurfaceRef) -> *mut c_void;
    fn IOSurfaceGetBytesPerRow(s: IOSurfaceRef) -> usize;
    fn IOSurfaceGetWidth(s: IOSurfaceRef) -> usize;
    fn IOSurfaceGetHeight(s: IOSurfaceRef) -> usize;
    fn IOSurfaceLookup(id: u32) -> IOSurfaceRef; // resolve a global IOSurface id (the engine's alloc id)
    fn IOSurfaceGetPixelFormat(buffer: IOSurfaceRef) -> u32;
    fn CFRelease(cf: *const c_void);
}

/// Release a CF/IOSurface reference.
pub unsafe fn cfrelease(s: IOSurfaceRef) {
    if !s.is_null() {
        CFRelease(s as *const c_void);
    }
}

// ---- GPU rung 2 mach-port handle bridge (see mach_bridge.c) ----
extern "C" {
    fn dd_mach_server_start(name: *const std::os::raw::c_char) -> std::os::raw::c_int;
    fn dd_mach_recv(
        out_id: *mut u32,
        out_generation: *mut u32,
        out_surface: *mut *mut c_void,
    ) -> std::os::raw::c_int;
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

/// The well-known bootstrap service name the engine sends IOSurface handles to.
pub const GPU_BRIDGE_SERVICE: &str = "com.dd.display.gpu";

/// The bootstrap service name for the IOSurface handle bridge. `DD_GPU_BRIDGE_NAME` overrides the
/// default so multiple dd-display instances can run concurrently (e.g. one per agent/benchmark) —
/// the engine reads the SAME env, so a guest's IOSurfaces reach the matching compositor. Default is
/// the historical `com.dd.display.gpu` (fully backward-compatible when the env is unset).
pub fn gpu_bridge_service() -> String {
    std::env::var("DD_GPU_BRIDGE_NAME").unwrap_or_else(|_| GPU_BRIDGE_SERVICE.to_string())
}

// id → IOSurfaceRef (as usize; a raw pointer isn't Send). Populated by the mach-receive thread; read by
// the compositor when a dmabuf buffer names an IOSurface id.
static GPU_SURFACES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u32, usize>>> =
    std::sync::OnceLock::new();

fn gpu_map() -> &'static std::sync::Mutex<std::collections::HashMap<u32, usize>> {
    GPU_SURFACES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// id → allocation generation. The IOSurface id is a macOS-minted `IOSurfaceGetID` that dd RECYCLES
// (a retired pool surface is re-handed to the next same-size alloc; a released legacy id is
// OS-recyclable), so the bare id cannot distinguish a live allocation from a stale reference. This
// table stamps each id's current allocation: set to 1 on first registration and bumped every time the
// id's cached surface is replaced by a different one. It is read into `IOSurfaceMetadata::generation`
// so the compositor can reject a guest dmabuf whose modifier carries a retired generation. Values stay
// in the 15-bit modifier field range (1..=0x7fff); 0 is reserved as "unversioned".
static GPU_GENERATIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u32, u32>>> =
    std::sync::OnceLock::new();

fn gpu_generations() -> &'static std::sync::Mutex<std::collections::HashMap<u32, u32>> {
    GPU_GENERATIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// The current allocation generation stamped for `id` (0 if the host is not tracking this id, e.g. a
/// legacy `IOSurfaceLookup` fallback). Exposed for the compositor's authentication path and tests.
pub fn iosurface_generation(id: u32) -> u32 {
    gpu_generations().lock().unwrap().get(&id).copied().unwrap_or(0)
}

/// Start the mach service + a background receive thread that caches every `(id → IOSurface)` the engine
/// sends. Returns true if the service registered. Call once before serving dmabuf clients.
pub fn start_gpu_bridge() -> bool {
    let svc = gpu_bridge_service();
    let name = std::ffi::CString::new(svc.clone()).unwrap();
    let rc = unsafe { dd_mach_server_start(name.as_ptr()) };
    if rc != 0 {
        eprintln!("dd-display: GPU mach bridge failed to register ({svc}): rc={rc}");
        return false;
    }
    eprintln!("dd-display: GPU mach bridge listening ({svc})");
    std::thread::spawn(|| loop {
        let mut id: u32 = 0;
        let mut generation: u32 = 0;
        let mut surf: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { dd_mach_recv(&mut id, &mut generation, &mut surf) };
        if rc != 0 || surf.is_null() {
            if rc != 0 {
                eprintln!("dd-display: GPU mach recv error rc={rc}");
            }
            continue;
        }
        // Cache the id's current surface, and record the ENGINE-supplied allocation generation. The
        // engine is the single authority: it stamps the generation at creation and bumps it on each pool
        // re-checkout (a recycled id handed to a fresh allocation), re-announcing here. Storing the
        // engine's value (rather than a locally-derived counter) keeps the guest's modifier generation
        // and the compositor's authenticated generation in lockstep, so a stale reference is rejected.
        let mut m = gpu_map().lock().unwrap();
        let replaced = m.insert(id, surf as usize);
        gpu_generations().lock().unwrap().insert(id, generation);
        if let Some(old) = replaced {
            unsafe { cfrelease(old as IOSurfaceRef) };
        }
        let (w, h) = unsafe { (IOSurfaceGetWidth(surf), IOSurfaceGetHeight(surf)) };
        eprintln!("dd-display: GPU bridge cached IOSurface id={id} gen={generation} size={w}x{h}");
    });
    true
}

/// Resolve an IOSurface id (from the dmabuf modifier) to a +1-retained surface: prefer the mach-bridge
/// cache (the engine's send-right), else the legacy global `IOSurfaceLookup` (restricted on modern macOS).
/// The caller `cfrelease`s the result.
pub unsafe fn resolve_iosurface(id: u32) -> IOSurfaceRef {
    if let Some(&p) = gpu_map().lock().unwrap().get(&id) {
        let s = p as IOSurfaceRef;
        CFRetain(s as *const c_void);
        return s;
    }
    IOSurfaceLookup(id)
}

pub fn iosurface_metadata(id: u32) -> Option<crate::present::IOSurfaceMetadata> {
    let surface = unsafe { resolve_iosurface(id) };
    if surface.is_null() {
        return None;
    }
    let width = unsafe { IOSurfaceGetWidth(surface) };
    let height = unsafe { IOSurfaceGetHeight(surface) };
    let bytes_per_row = unsafe { IOSurfaceGetBytesPerRow(surface) };
    let pixel_format = unsafe { IOSurfaceGetPixelFormat(surface) };
    // `resolve_iosurface` hands back a +1-retained surface (a `CFRetain` on the cache path or a
    // Create-rule reference from `IOSurfaceLookup`); balance it now that the metadata is read.
    // Omitting this leaked one reference on the cached surface per metadata query — and the
    // compositor queries metadata for every accelerated dmabuf frame, so the leak grew unboundedly.
    // The generation comes from the id→generation table, not the surface, so releasing here is safe.
    unsafe { cfrelease(surface) };
    Some(crate::present::IOSurfaceMetadata {
        width: width.try_into().ok()?,
        height: height.try_into().ok()?,
        bytes_per_row: bytes_per_row.try_into().ok()?,
        pixel_format,
        generation: iosurface_generation(id),
    })
}

const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241; // 'BGRA'

/// Allocate a host `IOSurface` (BGRA8888, `w`×`h`). This is the buffer whose *pages* a guest's
/// `linux-dmabuf` allocation would be backed by; the host wraps it as an `MTLTexture` with zero copy.
/// The caller owns it (release with `CFRelease`).
pub unsafe fn create_iosurface(w: u32, h: u32) -> IOSurfaceRef {
    let k = |s: CFStringRef| &*(s as *const NSString);
    let keys: [&NSString; 5] = [
        k(kIOSurfaceWidth),
        k(kIOSurfaceHeight),
        k(kIOSurfaceBytesPerElement),
        k(kIOSurfaceBytesPerRow),
        k(kIOSurfacePixelFormat),
    ];
    let vals = [
        NSNumber::numberWithInt(w as i32),
        NSNumber::numberWithInt(h as i32),
        NSNumber::numberWithInt(4),
        NSNumber::numberWithInt((w * 4) as i32),
        NSNumber::numberWithInt(PIXEL_FORMAT_BGRA),
    ];
    let props = NSDictionary::from_id_slice(&keys, &vals);
    IOSurfaceCreate(Retained::as_ptr(&props) as *const c_void)
}

// ===================== L4: async submit + cross-queue IOSurface tearing fence =====================
// The executor (dd-gpu IR → Metal render into the guest IOSurface) and the compositor (blit that
// IOSurface into the drawable/PNG target) run as TWO threads with TWO command queues in this one
// process. L4 removes the executor's per-frame `waitUntilCompleted` so the guest can build frame N+1
// while the GPU renders frame N (CPU/GPU overlap — the p50≈195µs serial GPU wait was the bottleneck).
//
// Correctness (ZERO tearing) rests on a bidirectional GPU fence, keyed by IOSurface id, between the two
// queues — which only works if BOTH queues live on the SAME MTLDevice, so we force one shared device.
//   render_ev:  executor SIGNALS gen g when render g into surface S completes; compositor WAITS >= g
//               before its blit → never samples a half-rendered surface (fence a).
//   present_ev: compositor SIGNALS gen g when its blit of surface S completes; executor WAITS >= g-1
//               before render g overwrites S → never overwrites a surface the compositor still reads
//               (fence b).
// The guest is paced ~1 frame ahead by the wl frame callback (it renders N+1 only after the compositor
// committed present N), so render_gen and the display gen stay in lockstep and no ring is needed: the
// two events serialize S's single producer/consumer precisely while the overlap comes from the async ack.
// Gated by env: default ON; set DD_RENDER_NOASYNC=1 to fall back to the old synchronous path (A/B).

/// One process-wide `MTLDevice` so the executor's and compositor's queues can share `MTLEvent`s. Stored
/// as a raw +1-retained pointer (leaked for process lifetime; a raw ptr isn't `Send`). `0` = no GPU.
static SHARED_DEVICE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// The shared system default `MTLDevice` (same object for every caller). `None` if Metal is unavailable.
pub fn shared_device() -> Option<Retained<ProtocolObject<dyn MTLDevice>>> {
    let raw = *SHARED_DEVICE.get_or_init(|| unsafe {
        let d = MTLCreateSystemDefaultDevice(); // +1 retained; keep this ref alive forever (process device)
        d as usize
    });
    if raw == 0 {
        return None;
    }
    unsafe { Retained::retain(raw as *mut ProtocolObject<dyn MTLDevice>) }
}

/// L4 async submit + fence gate. Default ON; `DD_RENDER_NOASYNC=1` restores the synchronous baseline.
pub fn async_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DD_RENDER_NOASYNC").is_none())
}

struct SurfFence {
    render_ev: usize, // *mut ProtocolObject<dyn MTLEvent>, +1 retained (leaked, one per surface id)
    present_ev: usize,
    render_gen: u64, // last render generation the executor reserved for this surface
}
static FENCES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u32, SurfFence>>> =
    std::sync::OnceLock::new();
fn fences() -> &'static std::sync::Mutex<std::collections::HashMap<u32, SurfFence>> {
    FENCES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}
fn new_event_raw() -> usize {
    match shared_device().and_then(|d| d.newEvent()) {
        Some(ev) => Retained::into_raw(ev) as usize, // leak one owning ref (process lifetime, ≤ #surfaces)
        None => 0,
    }
}

/// Executor: reserve the next render generation for surface `id` and get its fence events. Returns
/// `(render_ev, present_ev, gen, wait_present)`. The caller encodes `encodeWaitForEvent(present_ev,
/// wait_present)` at the START of the render command buffer and `encodeSignalEvent(render_ev, gen)` at
/// the END. Call exactly once per rendered frame (it advances the generation).
pub fn fence_begin_render(
    id: u32,
) -> Option<(
    Retained<ProtocolObject<dyn MTLEvent>>,
    Retained<ProtocolObject<dyn MTLEvent>>,
    u64,
    u64,
)> {
    let mut m = fences().lock().unwrap();
    let f = m.entry(id).or_insert_with(|| SurfFence {
        render_ev: new_event_raw(),
        present_ev: new_event_raw(),
        render_gen: 0,
    });
    if f.render_ev == 0 || f.present_ev == 0 {
        return None;
    }
    f.render_gen += 1;
    let gen = f.render_gen;
    let wait_present = gen.saturating_sub(1);
    let re = unsafe { Retained::retain(f.render_ev as *mut ProtocolObject<dyn MTLEvent>)? };
    let pe = unsafe { Retained::retain(f.present_ev as *mut ProtocolObject<dyn MTLEvent>)? };
    Some((re, pe, gen, wait_present))
}

/// Compositor: the fence events + the generation of surface `id` to display (the latest render the
/// executor committed). `None` if the executor hasn't rendered this surface yet. The caller encodes
/// `encodeWaitForEvent(render_ev, gen)` BEFORE its blit and `encodeSignalEvent(present_ev, gen)` AFTER.
pub fn fence_begin_present(
    id: u32,
) -> Option<(
    Retained<ProtocolObject<dyn MTLEvent>>,
    Retained<ProtocolObject<dyn MTLEvent>>,
    u64,
)> {
    let m = fences().lock().unwrap();
    let f = m.get(&id)?;
    if f.render_ev == 0 || f.present_ev == 0 || f.render_gen == 0 {
        return None;
    }
    let re = unsafe { Retained::retain(f.render_ev as *mut ProtocolObject<dyn MTLEvent>)? };
    let pe = unsafe { Retained::retain(f.present_ev as *mut ProtocolObject<dyn MTLEvent>)? };
    Some((re, pe, f.render_gen))
}

/// Release the fence for surface `id` (both `MTLEvent`s) and forget it. Call on client disconnect: the
/// departed compositor will never signal `present_ev` again, so a NEW client that reuses this IOSurface id
/// would inherit a stale `render_gen` and its render's `encodeWaitForEvent(present_ev, gen-1)` would block
/// forever (command buffer never completes → present `waitUntilCompleted` hangs). Dropping the entry both
/// stops the `MTLEvent` leak and forces the next `fence_begin_render` to start a fresh generation from 0.
pub fn fence_drop(id: u32) {
    if let Some(f) = fences().lock().unwrap().remove(&id) {
        // Reclaim the two leaked +1 owning refs so the MTLEvents are actually released.
        unsafe {
            if f.render_ev != 0 {
                drop(Retained::from_raw(
                    f.render_ev as *mut ProtocolObject<dyn MTLEvent>,
                ));
            }
            if f.present_ev != 0 {
                drop(Retained::from_raw(
                    f.present_ev as *mut ProtocolObject<dyn MTLEvent>,
                ));
            }
        }
    }
}

/// Release the cached IOSurface for `id` (the engine's send-right) and forget it. Call on client disconnect
/// so IOSurfaces don't leak as clients churn — the mach bridge caches one +1-retained surface per id.
pub fn gpu_surface_drop(id: u32) {
    if let Some(p) = gpu_map().lock().unwrap().remove(&id) {
        unsafe { cfrelease(p as IOSurfaceRef) };
    }
}

/// The shared host Metal context: one device + one command queue. Cloneable handle intent — hold one and
/// pass `&MetalCtx` around. Reused by both the display present path and (by design) the `dd-gpu` backend.
pub struct MetalCtx {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

impl MetalCtx {
    /// Create from the system default GPU. `None` if Metal is unavailable (no GPU / not macOS-GPU).
    /// Uses the ONE process-shared `MTLDevice` (see `shared_device`) so the executor's and compositor's
    /// command queues can be synchronized with cross-queue `MTLEvent`s (L4 tearing fence).
    pub fn new() -> Option<MetalCtx> {
        let device = shared_device()?;
        let queue = device.newCommandQueue()?;
        Some(MetalCtx { device, queue })
    }

    /// Build a context around an already-created device + queue — the seam `dd-gpu`'s `MetalBackend`
    /// uses so the GPU executor and the compositor share ONE device (no cross-device texture copies).
    pub fn from_device(
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    ) -> MetalCtx {
        MetalCtx { device, queue }
    }

    /// A `BGRA8Unorm` texture, optionally used as a render/blit target and readable back on the CPU
    /// (`storageModeShared`, which is unified memory on Apple Silicon → the readback is free).
    pub fn new_bgra_texture(&self, w: u32, h: u32) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                w as usize,
                h as usize,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Shared);
        self.device
            .newTextureWithDescriptor(&desc)
            .expect("newTextureWithDescriptor failed")
    }

    /// Wrap a host `IOSurface` as an `MTLTexture` with **zero copy** — the IOSurface's pages ARE the
    /// texture's storage. This is the GPU-rung-2 mechanism: a guest's IOSurface-backed `linux-dmabuf`
    /// buffer becomes a compositable texture with no upload and no readback (exactly Chrome's dmabuf path).
    pub fn texture_from_iosurface(
        &self,
        surface: IOSurfaceRef,
        w: u32,
        h: u32,
    ) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                w as usize,
                h as usize,
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

    /// GPU rung 3 first slice: the guest RENDERS on the host GPU. Run a real Metal **render pass** into
    /// the (rung-2) IOSurface — clear + a rasterized RGB triangle via compiled MSL vertex/fragment shaders.
    /// This is the host executor's core op; a forwarded `dd-gpu` IR `BeginRenderPass`/`Draw` maps onto it.
    /// (Shaders are compiled per call for the slice; caching is a follow-up.)
    pub fn render_triangle_into(&self, target: &ProtocolObject<dyn MTLTexture>) {
        const SRC: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct VOut { float4 pos [[position]]; float4 color; };
vertex VOut vmain(uint vid [[vertex_id]]) {
    float2 p[3] = { float2(0.0, 0.7), float2(-0.7, -0.6), float2(0.7, -0.6) };
    float3 c[3] = { float3(1.0,0.2,0.2), float3(0.2,1.0,0.2), float3(0.2,0.4,1.0) };
    VOut o; o.pos = float4(p[vid], 0.0, 1.0); o.color = float4(c[vid], 1.0); return o;
}
fragment float4 fmain(VOut in [[stage_in]]) { return in.color; }
"#;
        let lib = self
            .device
            .newLibraryWithSource_options_error(&NSString::from_str(SRC), None)
            .expect("MSL compile failed");
        let vfn = lib
            .newFunctionWithName(&NSString::from_str("vmain"))
            .expect("vmain");
        let ffn = lib
            .newFunctionWithName(&NSString::from_str("fmain"))
            .expect("fmain");
        let pdesc = MTLRenderPipelineDescriptor::new();
        pdesc.setVertexFunction(Some(&vfn));
        pdesc.setFragmentFunction(Some(&ffn));
        unsafe {
            pdesc
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        }
        let pipeline = self
            .device
            .newRenderPipelineStateWithDescriptor_error(&pdesc)
            .expect("pipeline");

        let pass = unsafe { MTLRenderPassDescriptor::renderPassDescriptor() };
        let ca = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(target));
        ca.setLoadAction(MTLLoadAction::Clear);
        ca.setClearColor(MTLClearColor {
            red: 0.05,
            green: 0.06,
            blue: 0.12,
            alpha: 1.0,
        });
        ca.setStoreAction(MTLStoreAction::Store);

        let cmd = self.queue.commandBuffer().expect("commandBuffer");
        let enc = cmd
            .renderCommandEncoderWithDescriptor(&pass)
            .expect("render encoder");
        enc.setRenderPipelineState(&pipeline);
        unsafe { enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3) };
        enc.endEncoding();
        cmd.commit();
        unsafe { cmd.waitUntilCompleted() };
    }

    /// GPU blit `src` → `dst` (same size), synchronously.
    pub fn blit(&self, src: &ProtocolObject<dyn MTLTexture>, dst: &ProtocolObject<dyn MTLTexture>) {
        let cmd = self.queue.commandBuffer().expect("commandBuffer");
        let enc = cmd.blitCommandEncoder().expect("blitCommandEncoder");
        unsafe { enc.copyFromTexture_toTexture(src, dst) };
        enc.endEncoding();
        cmd.commit();
        unsafe { cmd.waitUntilCompleted() };
    }

    /// L4 compositor blit of a guest IOSurface (`src`), guarded by the cross-queue tearing fence. `src`
    /// is the zero-copy wrap of guest IOSurface id `surface_id`; the executor renders into that same
    /// surface on another queue. We WAIT on `render_ev >= gen` so the blit never samples a half-rendered
    /// surface (fence a), then SIGNAL `present_ev = gen` on completion so the executor may safely overwrite
    /// the surface for a later frame (fence b). If no fence is registered yet (executor hasn't rendered
    /// this surface) we fall back to a plain blit. Kept synchronous (the compositor's own pacing); the
    /// waits/signals are GPU-side so this does not stall the guest.
    pub fn blit_fenced(
        &self,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
        surface_id: u32,
    ) {
        self.blit_fenced_ex(src, dst, surface_id, true);
    }

    /// Fenced compositor blit with an explicit `wait_completion`. When `false` (the default headless
    /// benchmark + windowed present path), the CPU does NOT stall on GPU completion — the cross-queue
    /// fence (executor waits `present_ev >= gen-1` before overwriting the surface, see metal_backend
    /// `submit`) guarantees ordering, so the guest's frame-callback fires as soon as the blit is
    /// *encoded*, not *finished*. This is the compositor-side analog of L4: it removes the executor-GPU-
    /// render + compositor-blit stall from the guest's per-frame critical path (the frame callback is
    /// sent right after `present()` returns). `true` (only when a synchronous readback follows, e.g. a
    /// PNG-sample frame) blocks until the pixels are ready.
    pub fn blit_fenced_ex(
        &self,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
        surface_id: u32,
        wait_completion: bool,
    ) {
        let fence = if async_on() {
            fence_begin_present(surface_id)
        } else {
            None
        };
        let cmd = self.queue.commandBuffer().expect("commandBuffer");
        if let Some((render_ev, _present_ev, gen)) = &fence {
            cmd.encodeWaitForEvent_value(render_ev, *gen); // fence a: don't read S until render(gen) done
        }
        let enc = cmd.blitCommandEncoder().expect("blitCommandEncoder");
        unsafe { enc.copyFromTexture_toTexture(src, dst) };
        enc.endEncoding();
        if let Some((_render_ev, present_ev, gen)) = &fence {
            cmd.encodeSignalEvent_value(present_ev, *gen); // fence b: S free for the executor to overwrite
        }
        cmd.commit();
        if wait_completion {
            unsafe { cmd.waitUntilCompleted() };
        }
    }

    /// Read a `BGRA8Unorm` texture back to CPU bytes (BGRA, tight).
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

    /// The full display CPU→GPU→CPU round-trip used to PROVE the Metal path: upload the committed buffer
    /// to a GPU texture, GPU-blit it to an offscreen target, and read the target back. Returns RGBA
    /// (B/R swapped, opaque) for PNG. If the bytes survive a real Metal upload+blit+readback, the
    /// hardware present path is sound.
    pub fn composite_to_rgba(&self, bgra: &[u8], w: u32, h: u32) -> Vec<u8> {
        let src = self.upload_bgra(bgra, w, h);
        let dst = self.new_bgra_texture(w, h);
        self.blit(&src, &dst);
        let out_bgra = self.readback_bgra(&dst, w, h);
        let mut rgba = vec![0u8; out_bgra.len()];
        for i in (0..out_bgra.len()).step_by(4) {
            rgba[i] = out_bgra[i + 2];
            rgba[i + 1] = out_bgra[i + 1];
            rgba[i + 2] = out_bgra[i];
            rgba[i + 3] = 0xff;
        }
        rgba
    }
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

/// A [`crate::present::Presenter`] that pushes each committed surface through the real Metal upload +
/// GPU blit + readback and dumps the GPU-rendered result to a PNG. This is the self-verifiable proof that
/// the hardware present path is correct (the pixels made a full CPU→GPU→CPU round-trip on a real
/// `MTLDevice`), inspectable without a screen.
pub struct MetalPngPresenter {
    ctx: MetalCtx,
    dir: std::path::PathBuf,
    pub frames: u32,
    pub last: Option<(u32, u32, u32, Vec<u8>)>, // (sid, w, h, rgba) — asserted by the selftest
    /// `DD_DISPLAY_PNG_EVERY`: readback+encode+write a PNG only every Nth committed frame (default 1 =
    /// every frame). The GPU composite blit (hop 15) still runs every frame — sampling only elides the
    /// expensive CPU readback+PNG so the compositor keeps pace with the render pipeline during benchmarks
    /// (otherwise per-frame PNG encoding, not the pipeline, would bound the measured FPS).
    png_every: u32,
    /// Persistent composite target per (sid,w,h). Reused across frames so the steady-state present hop is
    /// a zero-allocation fenced blit (the old per-frame `new_bgra_texture` was a benchmark artifact — the
    /// real windowed present blits into a persistent CAMetalLayer drawable, never a fresh texture/frame).
    dst_cache: std::collections::HashMap<u32, (u32, u32, Retained<ProtocolObject<dyn MTLTexture>>)>,
    /// `DD_DISPLAY_SYNC_PRESENT=1` restores the old synchronous compositor blit (CPU stalls on GPU
    /// completion every frame) for A/B attribution of the present hop. Default OFF (async, fence-gated).
    sync_present: bool,
}

impl MetalPngPresenter {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Option<MetalPngPresenter> {
        let png_every = std::env::var("DD_DISPLAY_PNG_EVERY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1u32)
            .max(1);
        let sync_present = std::env::var_os("DD_DISPLAY_SYNC_PRESENT").is_some();
        Some(MetalPngPresenter {
            ctx: MetalCtx::new()?,
            dir: dir.into(),
            frames: 0,
            last: None,
            png_every,
            dst_cache: std::collections::HashMap::new(),
            sync_present,
        })
    }
}

impl crate::present::Presenter for MetalPngPresenter {
    fn frame_count(&self) -> u32 {
        self.frames
    }
    fn present(
        &mut self,
        surf: &crate::present::SurfaceBuffer,
    ) -> Result<crate::present::PresentOutcome, crate::present::PresentError> {
        let (w, h) = (surf.width as u32, surf.height as u32);
        let dump = self.frames % self.png_every == 0; // sample: only some frames pay readback+PNG
                                                      // GPU rung 2: an IOSurface-backed dmabuf composites zero-copy (wrap → GPU-blit → readback); an
                                                      // shm buffer uploads its bytes first. The GPU blit (hop 15) runs EVERY frame; only sampled frames
                                                      // pay the CPU readback + PNG encode.
        let rgba = match surf.iosurface_id {
            Some(id) => {
                let surface = unsafe { resolve_iosurface(id) };
                if surface.is_null() {
                    return Err(crate::present::PresentError::Device(format!(
                        "IOSurface id {id} not found"
                    )));
                }
                let src = self.ctx.texture_from_iosurface(surface, w, h);
                if surf.gpu_render && std::env::var_os("DD_DISPLAY_TEST_TRIANGLE").is_some() {
                    self.ctx.render_triangle_into(&src); // rung 3: host GPU renders into the guest IOSurface
                }
                // Reuse a persistent composite target for this surface (zero per-frame allocation).
                let dst = {
                    let e = self.dst_cache.entry(surf.sid);
                    let slot = e.or_insert_with(|| (0, 0, self.ctx.new_bgra_texture(w, h)));
                    if slot.0 != w || slot.1 != h {
                        *slot = (w, h, self.ctx.new_bgra_texture(w, h));
                    }
                    slot.2.clone()
                };
                // L4/L6: fenced blit. Async (default) — the frame-callback that paces the guest fires as
                // soon as the blit is ENCODED, not GPU-finished; the cross-queue fence keeps it tearing-free.
                // Sync only on a PNG-sample frame (a CPU readback follows) or under DD_DISPLAY_SYNC_PRESENT.
                let wait = dump || self.sync_present;
                self.ctx.blit_fenced_ex(&src, &dst, id, wait);
                unsafe { cfrelease(surface) };
                if !dump {
                    self.frames += 1;
                    // Composited on-GPU (fenced blit encoded); skip the CPU readback+PNG this frame.
                    return Ok(crate::present::PresentOutcome::Delivered {
                        serial: self.frames as u64,
                        timing: None,
                    });
                }
                let bgra = self.ctx.readback_bgra(&dst, w, h);
                let mut out = vec![0u8; bgra.len()];
                for i in (0..bgra.len()).step_by(4) {
                    out[i] = bgra[i + 2];
                    out[i + 1] = bgra[i + 1];
                    out[i + 2] = bgra[i];
                    out[i + 3] = 0xff;
                }
                out
            }
            None => {
                if !dump {
                    self.frames += 1;
                    return Ok(crate::present::PresentOutcome::Delivered {
                        serial: self.frames as u64,
                        timing: None,
                    });
                }
                self.ctx.composite_to_rgba(&surf.bgra, w, h)
            }
        };
        let png = dd_term_core::png::encode_rgba(w, h, &rgba);
        std::fs::create_dir_all(&self.dir).map_err(crate::present::PresentError::Output)?;
        // Frame-indexed so an animated client's successive frames are all captured (not overwritten).
        let path = self
            .dir
            .join(format!("surface-{}-{:03}.png", surf.sid, self.frames));
        std::fs::write(&path, png).map_err(crate::present::PresentError::Output)?;
        self.frames += 1;
        self.last = Some((surf.sid, w, h, rgba));
        if display_debug() {
            eprintln!(
                "[dd-display/metal] GPU-composited sid={} {}x{} -> {}",
                surf.sid,
                w,
                h,
                path.display()
            );
        }
        Ok(crate::present::PresentOutcome::Delivered {
            serial: self.frames as u64,
            timing: None,
        })
    }
}

/// `dd-display selftest-iosurface <out.png>`: prove the **zero-copy GPU-rung-2 path**. Allocate a host
/// `IOSurface`, write a pattern into its pages via `IOSurfaceGetBaseAddress` (CPU), wrap it as an
/// `MTLTexture` (NO upload — the IOSurface pages back the texture), GPU-blit it to an offscreen target,
/// and read that back → PNG. The pattern round-trips with no `replaceRegion` copy in the path, so the
/// GPU composited the IOSurface directly — the mechanism a guest's IOSurface-backed dmabuf uses.
pub fn selftest_iosurface(out: &str) -> ! {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-iosurface: no Metal device");
        std::process::exit(1);
    };
    let (w, h): (u32, u32) = (256, 160);
    unsafe {
        let surf = create_iosurface(w, h);
        if surf.is_null() {
            eprintln!("selftest-iosurface: IOSurfaceCreate failed");
            std::process::exit(1);
        }
        // Fill the IOSurface's pages directly (CPU), the weston `(x^y)` pattern in BGRA.
        IOSurfaceLock(surf, 0, std::ptr::null_mut());
        let base = IOSurfaceGetBaseAddress(surf) as *mut u8;
        let stride = IOSurfaceGetBytesPerRow(surf);
        for y in 0..h {
            for x in 0..w {
                let o = y as usize * stride + x as usize * 4;
                let (b, g, r);
                if x < 8 || x >= w - 8 || y < 8 || y >= h - 8 {
                    r = 0xff;
                    g = (x * 255 / w) as u8;
                    b = (y * 255 / h) as u8;
                } else {
                    let v = ((x ^ y) & 0xff) as u8;
                    r = v;
                    g = v;
                    b = v;
                }
                *base.add(o) = b;
                *base.add(o + 1) = g;
                *base.add(o + 2) = r;
                *base.add(o + 3) = 0;
            }
        }
        IOSurfaceUnlock(surf, 0, std::ptr::null_mut());

        // Wrap as a texture (zero-copy) and GPU-blit to an offscreen target, then read that back.
        let tex = ctx.texture_from_iosurface(surf, w, h);
        let dst = ctx.new_bgra_texture(w, h);
        ctx.blit(&tex, &dst);
        let bgra = ctx.readback_bgra(&dst, w, h);
        // Verify a known interior pixel (x=100,y=50): v = 100^50 = 86.
        let o = (50 * w + 100) as usize * 4;
        let v = (100u32 ^ 50u32) as u8;
        let ok = bgra[o] == v && bgra[o + 1] == v && bgra[o + 2] == v;

        let mut rgba = vec![0u8; bgra.len()];
        for i in (0..bgra.len()).step_by(4) {
            rgba[i] = bgra[i + 2];
            rgba[i + 1] = bgra[i + 1];
            rgba[i + 2] = bgra[i];
            rgba[i + 3] = 0xff;
        }
        let png = dd_term_core::png::encode_rgba(w, h, &rgba);
        let _ = std::fs::write(out, png);
        CFRelease(surf);
        if ok {
            println!("selftest-iosurface: IOSurface composited zero-copy as MTLTexture -> {out}");
            std::process::exit(0);
        }
        eprintln!("selftest-iosurface: pattern mismatch after GPU round-trip");
        std::process::exit(1);
    }
}

/// `dd-display selftest-render <out.png>`: GPU rung 3 host-executor proof. Allocate a host IOSurface,
/// run a Metal **render pass** (clear + rasterized triangle) into it — the guest's forwarded render — then
/// read it back → PNG. Unlike the rung-2 selftests (which CPU-fill or blit), this proves the host GPU
/// *rasterizes* into the shared IOSurface (the render target a forwarded `dd-gpu` IR would drive).
pub fn selftest_render(out: &str) -> ! {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-render: no Metal device");
        std::process::exit(1);
    };
    let (w, h): (u32, u32) = (256, 256);
    unsafe {
        let surf = create_iosurface(w, h);
        if surf.is_null() {
            eprintln!("selftest-render: IOSurfaceCreate failed");
            std::process::exit(1);
        }
        let tex = ctx.texture_from_iosurface(surf, w, h);
        ctx.render_triangle_into(&tex); // GPU render pass INTO the IOSurface
                                        // Read the IOSurface back (it now holds GPU-rasterized pixels) and PNG it.
        let dst = ctx.new_bgra_texture(w, h);
        ctx.blit(&tex, &dst);
        let bgra = ctx.readback_bgra(&dst, w, h);
        CFRelease(surf);
        let mut rgba = vec![0u8; bgra.len()];
        for i in (0..bgra.len()).step_by(4) {
            rgba[i] = bgra[i + 2];
            rgba[i + 1] = bgra[i + 1];
            rgba[i + 2] = bgra[i];
            rgba[i + 3] = 0xff;
        }
        let png = dd_term_core::png::encode_rgba(w, h, &rgba);
        let _ = std::fs::write(out, png);
    }
    println!("selftest-render: GPU-rendered a triangle into an IOSurface -> {out}");
    std::process::exit(0);
}

/// `dd-display selftest-metal <out.png>`: drive one real client frame (a forked Wayland client backs an
/// shm pool and passes the fd via SCM_RIGHTS over a real socket) through the compositor into
/// [`MetalPngPresenter`], so the committed `wl_shm` buffer is uploaded to a `MTLTexture`, GPU-blitted, and
/// read back → PNG. Proves the hardware present path end to end on real Metal. Exits.
pub fn selftest_metal(out: &str) -> ! {
    let Some(presenter) = MetalPngPresenter::new(
        std::path::Path::new(out)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    ) else {
        eprintln!("selftest-metal: no Metal device available");
        std::process::exit(1);
    };
    let sock = format!("/tmp/dd-display-metal-{}.sock", unsafe { libc::getpid() });
    let lfd = crate::listen_unix(&sock).expect("bind");
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        crate::selftest::client(&sock);
        unsafe { libc::_exit(0) };
    }
    let cfd = loop {
        let fd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd >= 0 {
            break fd;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            std::process::exit(1);
        }
    };
    unsafe {
        let fl = libc::fcntl(cfd, libc::F_GETFL);
        libc::fcntl(cfd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    let mut server = crate::server::Server::new(cfd, presenter);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while server.presenter().frames == 0 && std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd: cfd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, 50) };
        if !server.pump().unwrap_or(false) {
            break;
        }
    }
    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        libc::close(cfd);
        libc::close(lfd);
    }
    let _ = std::fs::remove_file(&sock);
    let produced = std::path::Path::new(out)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("surface-6.png");
    if produced != std::path::Path::new(out) {
        let _ = std::fs::rename(&produced, out);
    }
    if server.presenter().frames > 0 {
        println!("selftest-metal: GPU-composited a frame on real Metal -> {out}");
        std::process::exit(0);
    }
    eprintln!("selftest-metal: no frame");
    std::process::exit(1);
}

#[cfg(test)]
mod iosurface_ref_tests {
    use super::*;

    extern "C" {
        fn CFGetRetainCount(cf: *const c_void) -> isize;
    }

    // C31: `iosurface_metadata` reads a `+1`-retained surface from `resolve_iosurface` and must
    // release it. This exercises the mach-bridge cache path (the `CFRetain` branch) so no window
    // server / Metal device is required — only the IOSurface framework, which creates a plain kernel
    // object headlessly. Before the fix each query leaked one reference; here we assert the surface's
    // CFGetRetainCount returns to its baseline after a burst of metadata queries.
    #[test]
    fn iosurface_metadata_does_not_leak_a_reference() {
        // Own a host IOSurface (+1). `create_iosurface` needs no display server.
        let s = unsafe { create_iosurface(8, 8) };
        assert!(!s.is_null(), "IOSurfaceCreate returned null (headless framework unavailable)");

        // Register it in the GPU-bridge cache under a test id, mirroring what the mach-recv thread
        // does: the map holds its own owning +1 reference.
        let id = 0x7f10_0001u32;
        unsafe { CFRetain(s as *const c_void) };
        gpu_map().lock().unwrap().insert(id, s as usize);
        gpu_generations().lock().unwrap().insert(id, 1);

        let baseline = unsafe { CFGetRetainCount(s as *const c_void) };
        for _ in 0..64 {
            let md = iosurface_metadata(id).expect("metadata for cached surface");
            assert_eq!(md.width, 8);
            assert_eq!(md.height, 8);
            assert_eq!(md.generation, 1);
        }
        let after = unsafe { CFGetRetainCount(s as *const c_void) };
        assert_eq!(
            after, baseline,
            "iosurface_metadata leaked {} reference(s) over 64 queries",
            after - baseline
        );

        // Cleanup: drop the cache's owning ref, then our own.
        if let Some(p) = gpu_map().lock().unwrap().remove(&id) {
            unsafe { cfrelease(p as IOSurfaceRef) };
        }
        gpu_generations().lock().unwrap().remove(&id);
        unsafe { cfrelease(s) };
    }
}
