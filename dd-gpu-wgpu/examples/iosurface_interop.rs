//! Zero-copy IOSurface interop proof for the wgpu backend (increment 2).
//!
//! Proves the single hardest piece of the wgpu host-GPU path: rendering through **wgpu** directly into
//! an **IOSurface-backed `MTLTexture`** (the same `newTextureWithDescriptor:iosurface:plane:` wrap
//! `dd-display`'s `MetalCtx::texture_from_iosurface` uses), then reading the pixels back **through the
//! IOSurface** (CPU map of the shared pages) — NOT through a wgpu copy. If the render lands in the
//! IOSurface with no intermediate copy, the zero-copy present path is real.
//!
//! The recipe, and the exact objc2-metal <-> metal-rs raw-pointer seam it rests on:
//!
//! 1. `wgpu::Device` (own system-Metal device via `WgpuBackend::new`). Its underlying `MTLDevice` is
//!    extracted with `Device::as_hal::<wgpu_hal::api::Metal>()` -> `hal_device.raw_device().lock().as_ptr()`
//!    (a `metal::Device`, i.e. metal-rs 0.31, raw `*mut MTLDevice`). Sharing dd-display's *exact* device
//!    the other way (`device_from_raw`) is not reachable through wgpu-hal 24's public API — see the report
//!    at the bottom of `main`. Extracting wgpu's device and building the IOSurface texture on THAT device
//!    gives the same guarantee that matters for zero-copy: producer and consumer share one `MTLDevice`.
//! 2. Wrap that raw `MTLDevice` on the **objc2** side (`Retained::retain`, the type dd-display speaks) and
//!    create an `IOSurface` + an IOSurface-backed `MTLTexture` on it — byte-for-byte the dd-display recipe
//!    (BGRA8Unorm, `.ShaderRead | .RenderTarget`, `storageMode = .Shared`).
//! 3. Bridge that `Retained<ProtocolObject<dyn MTLTexture>>` (objc2) to a metal-rs `metal::Texture`
//!    (`Retained::as_ptr` -> `metal::TextureRef::from_ptr` -> `.to_owned()` = `retain +1`), then
//!    `wgpu_hal::metal::Device::texture_from_raw(...)` -> `wgpu::Device::create_texture_from_hal::<Metal>()`.
//! 4. Register it as the executor's render target, replay the increment-1 flat-quad IR into it via wgpu.
//! 5. `IOSurfaceLock` + `IOSurfaceGetBaseAddress` on the dd side, read the shared pages, assert the pixels.
//!
//! Run on the macOS host: `cargo run --release -p dd-gpu-wgpu --example iosurface_interop` (exit 0 = PASS).

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("iosurface_interop: macOS/Metal only");
}

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(imp::run());
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    use dd_gpu::ir::*;
    use dd_gpu_wgpu::WgpuBackend;

    use metal::foreign_types::{ForeignType, ForeignTypeRef};
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};
    use objc2_metal::{
        MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
    };

    // ---- IOSurface FFI (mirrors dd-display/src/metal.rs; declared here so the proof is self-contained) ----
    type IOSurfaceRef = *mut c_void;
    type CFStringRef = *const c_void;
    const KIO_SURFACE_LOCK_READ_ONLY: u32 = 0x0000_0001;
    const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241; // 'BGRA'

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
        fn CFRelease(cf: *const c_void);
    }

    /// Allocate a BGRA8888 host IOSurface (dd-display `create_iosurface`, inlined).
    unsafe fn create_iosurface(w: u32, h: u32) -> IOSurfaceRef {
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

    /// Wrap an IOSurface as a BGRA8Unorm `MTLTexture` on `device`, exactly as
    /// `MetalCtx::texture_from_iosurface` does (ShaderRead | RenderTarget, storageMode Shared, plane 0).
    unsafe fn texture_from_iosurface(
        device: &ProtocolObject<dyn MTLDevice>,
        surface: IOSurfaceRef,
        w: u32,
        h: u32,
    ) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let desc = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            w as usize,
            h as usize,
            false,
        );
        desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Shared);
        // newTextureWithDescriptor:iosurface:plane: — not bound by objc2-metal 0.2, so raw-send it.
        msg_send_id![device, newTextureWithDescriptor: &*desc, iosurface: surface, plane: 0usize]
    }

    const W: u32 = 64;
    const H: u32 = 64;

    pub fn run() -> i32 {
        // ---- 1. wgpu device over system Metal; extract its raw MTLDevice via wgpu-hal `as_hal`. --------
        let mut be = match WgpuBackend::new() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("iosurface_interop: no wgpu Metal device: {e:?}");
                return 1;
            }
        };
        // `as_hal` hands us `&wgpu_hal::metal::Device`; `raw_device()` is its metal-rs `Mutex<metal::Device>`.
        // `.as_ptr()` (foreign_types::ForeignType) is the raw `*mut MTLDevice` — the objc2 <-> metal-rs seam
        // on the DEVICE. Returned as usize (a raw ptr isn't Send; used immediately on this thread).
        let raw_device: usize = unsafe {
            be.device().as_hal::<wgpu_hal::api::Metal, _, _>(|hal| {
                let hal = hal.expect("wgpu device is not Metal");
                hal.raw_device().lock().as_ptr() as usize
            })
        };
        if raw_device == 0 {
            eprintln!("iosurface_interop: null MTLDevice from as_hal");
            return 1;
        }
        // objc2 view of the SAME device (retain +1 for our owned handle).
        let device_objc: Retained<ProtocolObject<dyn MTLDevice>> =
            unsafe { Retained::retain(raw_device as *mut ProtocolObject<dyn MTLDevice>) }
                .expect("retain MTLDevice");

        // ---- 2. IOSurface + IOSurface-backed MTLTexture on wgpu's device (the dd-display wrap). --------
        let surface = unsafe { create_iosurface(W, H) };
        if surface.is_null() {
            eprintln!("iosurface_interop: IOSurfaceCreate failed");
            return 1;
        }
        let tex_objc = unsafe { texture_from_iosurface(&device_objc, surface, W, H) };

        // ---- 3. Bridge objc2 MTLTexture -> metal-rs Texture -> wgpu-hal -> wgpu. -----------------------
        // `Retained::as_ptr` is a BORROWED raw id (no retain). `TextureRef::from_ptr` borrows it; `.to_owned()`
        // does `retain +1` (metal-rs `obj_clone`), so the metal-rs `Texture` owns an independent +1 that
        // wgpu-hal will `release` on drop. Our `tex_objc` keeps its own +1; the IOSurface keeps the storage.
        let raw_tex = Retained::as_ptr(&tex_objc) as *mut c_void;
        let mtl_texture: metal::Texture =
            unsafe { metal::TextureRef::from_ptr(raw_tex.cast()).to_owned() };
        let hal_texture = unsafe {
            wgpu_hal::metal::Device::texture_from_raw(
                mtl_texture,
                wgpu::TextureFormat::Bgra8Unorm,
                metal::MTLTextureType::D2,
                1, // array_layers
                1, // mip_levels
                wgpu_hal::CopyExtent { width: W, height: H, depth: 1 },
            )
        };
        let wgpu_tex = unsafe {
            be.device().create_texture_from_hal::<wgpu_hal::api::Metal>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("dd-iosurface-target"),
                    size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    // Matches the MTLTexture's RenderTarget|ShaderRead usage. We render into it; we do NOT
                    // read it back through wgpu (that would be a copy) — the readback is via the IOSurface.
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };

        // ---- 4. Register as the render target and replay the flat-quad IR through wgpu. ---------------
        be.set_render_target(1, wgpu_tex, TextureFormat::Bgra8Unorm);
        if let Err(e) = render_flat_quad(&mut be) {
            eprintln!("iosurface_interop: replay failed: {e:?}");
            unsafe { CFRelease(surface) };
            return 1;
        }
        // Ensure the GPU render into the IOSurface has completed before the CPU reads its pages.
        let _ = be.device().poll(wgpu::Maintain::Wait);

        // ---- 5. Read back THROUGH the IOSurface (shared pages), not a wgpu copy. -----------------------
        let (center, corner) = unsafe {
            IOSurfaceLock(surface, KIO_SURFACE_LOCK_READ_ONLY, std::ptr::null_mut());
            let base = IOSurfaceGetBaseAddress(surface) as *const u8;
            let stride = IOSurfaceGetBytesPerRow(surface);
            let px = |x: u32, y: u32| -> [u8; 4] {
                let o = y as usize * stride + x as usize * 4;
                [*base.add(o), *base.add(o + 1), *base.add(o + 2), *base.add(o + 3)]
            };
            let c = (px(32, 32), px(2, 2));
            IOSurfaceUnlock(surface, KIO_SURFACE_LOCK_READ_ONLY, std::ptr::null_mut());
            c
        };

        // BGRA byte order in the IOSurface: [B, G, R, A]. Red quad -> B=0 G=0 R=255; gray clear (0.1) -> ~26.
        let is_red = center[0] < 16 && center[1] < 16 && center[2] > 240;
        let is_gray = (20..=40).contains(&corner[0])
            && (20..=40).contains(&corner[1])
            && (20..=40).contains(&corner[2]);
        println!("[iosurface] center(32,32) BGRA={center:?}  corner(2,2) BGRA={corner:?}");
        println!("[iosurface] center is red (through IOSurface): {is_red}");
        println!("[iosurface] corner is gray clear (through IOSurface): {is_gray}");

        // Drop order: wgpu_tex/hal (release its +1), tex_objc (release its +1), then the IOSurface.
        drop(be);
        drop(tex_objc);
        unsafe { CFRelease(surface) };

        if is_red && is_gray {
            println!("\n==== iosurface_interop: PASS (zero-copy render landed in the IOSurface) ====");
            0
        } else {
            println!("\n==== iosurface_interop: FAIL ====");
            1
        }
    }

    /// Increment-1 flat-quad content: gray clear + a centered solid-red quad, rendered into whatever
    /// texture is registered as target id 1. Identical shape to `examples/verify_ir.rs` case 1, but the
    /// color target is BGRA8Unorm (the IOSurface's format).
    fn render_flat_quad(be: &mut WgpuBackend) -> dd_gpu::Result<()> {
        fn f32s(out: &mut Vec<u8>, vals: &[f32]) {
            for v in vals {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        fn pack_msl(src: &str) -> Vec<u32> {
            // MSL-as-bytes (legacy shim shape) -> forces the builtin-WGSL FLAT fallback in the backend.
            let mut words = vec![src.len() as u32];
            for ch in src.as_bytes().chunks(4) {
                let mut w = [0u8; 4];
                w[..ch.len()].copy_from_slice(ch);
                words.push(u32::from_le_bytes(w));
            }
            words
        }
        let rt = [2.0 / W as f32, -1.0, 2.0 / H as f32, -1.0];

        let mut verts = Vec::new();
        let corners = [(16.0, 16.0), (16.0, 48.0), (48.0, 16.0), (48.0, 16.0), (16.0, 48.0), (48.0, 48.0)];
        for (x, y) in corners {
            f32s(&mut verts, &[x, y]);
            f32s(&mut verts, &[1.0, 0.0, 0.0, 1.0]);
        }
        let mut unif = Vec::new();
        f32s(&mut unif, &rt);

        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: verts.len() as u64, usage: buffer_usage::VERTEX, label: "v".into() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: verts },
            Cmd::CreateBuffer(12, BufferDesc { size: unif.len() as u64, usage: buffer_usage::UNIFORM, label: "u".into() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: unif.clone() },
            Cmd::CreateShader { id: 20, kind: dd_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl("#include <metal_stdlib>\n[[stage_in]] flat") },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 24,
                    step_mode: 0,
                    attrs: vec![
                        VertexAttr { location: 0, format: 2, offset: 0 },
                        VertexAttr { location: 1, format: 4, offset: 8 },
                    ],
                }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: 0xf }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: "flat".into(),
            }),
            Cmd::CreateBindGroup(40, BindGroupDesc {
                set: 0,
                entries: vec![BindEntry { binding: 1, resource: BindResource::Buffer { id: 12, offset: 0, size: unif.len() as u64 } }],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.1, 0.1, 0.1, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: W as f32, h: H as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w: W, h: H },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: 6, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let ir = encode_stream(&cmds);
        dd_gpu::replay::replay_stream(be, &ir).map(|_| ())
    }
}
