//! macOS native presentation allocation: IOSurface storage wrapped by Metal, then imported into wgpu.

use std::ffi::c_void;

use hl_gpu::protocol::model::descriptor::TextureDesc;
use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
use hl_gpu::{GpuError, Result};
use hl_iosurface::Surface;
use metal::foreign_types::ForeignType;

use crate::device::Gpu;

type IOSurfaceRef = *mut c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_msgSend();
    fn sel_registerName(name: *const std::ffi::c_char) -> *const c_void;
}

/// Proven ability to allocate and import IOSurface-backed Metal textures on this executor's device.
#[derive(Clone, Copy)]
pub struct Allocator;

impl Allocator {
    pub fn new(gpu: &Gpu) -> Result<Self> {
        let descriptor = wgpu::TextureDescriptor {
            label: Some("hl-iosurface-probe"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        };
        let _ = Self.texture(gpu, &descriptor)?;
        Ok(Self)
    }

    pub fn supports(
        &self,
        desc: &TextureDesc,
        dimension: wgpu::TextureDimension,
        view: wgpu::TextureViewDimension,
        mip_levels: u32,
        sample_count: u32,
    ) -> bool {
        desc.dim == TextureDim::D2
            && desc.depth <= 1
            && desc.format == TextureFormat::Bgra8Unorm
            && desc.usage & texture_usage::PRESENT != 0
            && dimension == wgpu::TextureDimension::D2
            && view == wgpu::TextureViewDimension::D2
            && mip_levels == 1
            && sample_count == 1
    }

    pub fn texture(
        &self,
        gpu: &Gpu,
        descriptor: &wgpu::TextureDescriptor<'_>,
    ) -> Result<(wgpu::Texture, Surface)> {
        let surface = Surface::new_bgra(descriptor.size.width, descriptor.size.height)
            .map_err(|_| GpuError::Unsupported("iosurface: allocation failed"))?;
        let raw = metal_texture(gpu, &surface, descriptor)?;
        let hal = unsafe {
            // SAFETY: `raw` was created by this wgpu device's exact MTLDevice, its descriptor matches the
            // wgpu descriptor below, IOSurface allocation initializes its storage, and ownership transfers
            // into the HAL texture.
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                descriptor.format,
                metal::MTLTextureType::D2,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width: descriptor.size.width,
                    height: descriptor.size.height,
                    depth: 1,
                },
            )
        };
        let texture = unsafe {
            // SAFETY: the HAL texture was created from this same device and exactly matches `descriptor`.
            gpu.device
                .create_texture_from_hal::<wgpu::hal::api::Metal>(hal, descriptor)
        };
        Ok((texture, surface))
    }
}

fn metal_texture(
    gpu: &Gpu,
    surface: &Surface,
    descriptor: &wgpu::TextureDescriptor<'_>,
) -> Result<metal::Texture> {
    let texture_descriptor = metal::TextureDescriptor::new();
    texture_descriptor.set_texture_type(metal::MTLTextureType::D2);
    texture_descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
    texture_descriptor.set_width(descriptor.size.width as u64);
    texture_descriptor.set_height(descriptor.size.height as u64);
    texture_descriptor.set_depth(1);
    texture_descriptor.set_array_length(1);
    texture_descriptor.set_mipmap_level_count(1);
    texture_descriptor.set_sample_count(1);
    texture_descriptor.set_storage_mode(metal::MTLStorageMode::Shared);
    texture_descriptor.set_usage(
        metal::MTLTextureUsage::ShaderRead
            | metal::MTLTextureUsage::ShaderWrite
            | metal::MTLTextureUsage::RenderTarget,
    );

    let texture = unsafe {
        gpu.device.as_hal::<wgpu::hal::api::Metal, _, _>(|device| {
            let device =
                device.ok_or(GpuError::Unsupported("iosurface: wgpu device is not Metal"))?;
            let device = device.raw_device().lock();
            type NewTexture = unsafe extern "C" fn(
                *mut metal::MTLDevice,
                *const c_void,
                *mut metal::MTLTextureDescriptor,
                IOSurfaceRef,
                usize,
            ) -> *mut metal::MTLTexture;
            let selector = sel_registerName(c"newTextureWithDescriptor:iosurface:plane:".as_ptr());
            let call: NewTexture = std::mem::transmute(objc_msgSend as *const ());
            let raw = call(
                metal::Device::as_ptr(&device),
                selector,
                metal::TextureDescriptor::as_ptr(&texture_descriptor),
                surface.handle().as_ptr(),
                0,
            );
            if raw.is_null() {
                Err(GpuError::Unsupported(
                    "iosurface: Metal refused IOSurface texture",
                ))
            } else {
                // SAFETY: `newTexture...` returns an owned (+1) MTLTexture.
                Ok(metal::Texture::from_ptr(raw))
            }
        })
    };
    texture
}

#[cfg(test)]
mod tests {
    use hl_gpu::protocol::model::capability::PresentKind;
    use hl_gpu::protocol::model::command::Cmd;
    use hl_gpu::protocol::model::descriptor::{SurfaceDesc, TextureDesc};
    use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
    use hl_gpu::runtime::model::resources::SessionResources;
    use hl_gpu::runtime::port::executor::GpuExecutor;

    use crate::{DeviceConfig, WgpuExecutor};

    fn texture(format: TextureFormat) -> TextureDesc {
        TextureDesc {
            width: 4,
            height: 3,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format,
            usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT,
            label: String::new(),
        }
    }

    #[test]
    fn native_constructor_advertises_only_constructed_path() {
        let executor = WgpuExecutor::new_iosurface(DeviceConfig::default())
            .expect("Metal must construct an IOSurface texture");
        assert_eq!(
            executor.caps.present_kinds,
            vec![PresentKind::Shm, PresentKind::IoSurface]
        );
        assert_eq!(
            WgpuExecutor::capabilities_for(
                "test",
                false,
                wgpu::Features::empty(),
                wgpu::DownlevelFlags::empty(),
            )
            .present_kinds,
            vec![PresentKind::Shm]
        );
    }

    #[test]
    fn wgpu_renders_into_iosurface_storage() {
        let executor = WgpuExecutor::new_iosurface(DeviceConfig::default())
            .expect("Metal must construct an IOSurface texture");
        let texture = executor
            .make_texture(&texture(TextureFormat::Bgra8Unorm))
            .expect("BGRA presentation target");
        let surface = texture
            .iosurface
            .as_ref()
            .expect("eligible target must own IOSurface");
        assert_ne!(surface.id(), 0);

        let mut encoder =
            executor
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("hl-iosurface-test"),
                });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-iosurface-test"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        executor.gpu.queue.submit(Some(encoder.finish()));
        executor.wait_for_completion();

        for pixel in surface
            .read_bgra()
            .expect("read exact pixels")
            .chunks_exact(4)
        {
            assert_eq!(pixel, [0, 0, 255, 255]);
        }
    }

    #[test]
    fn unsupported_texture_shape_retains_shm_fallback() {
        let executor = WgpuExecutor::new_iosurface(DeviceConfig::default())
            .expect("Metal must construct an IOSurface texture");
        let texture = executor
            .make_texture(&texture(TextureFormat::Rgba8Unorm))
            .expect("ordinary portable texture");
        assert!(texture.iosurface.is_none());
    }

    #[test]
    fn present_hands_off_completion_without_waiting_and_retains_surface() {
        let mut executor = WgpuExecutor::new_iosurface(DeviceConfig::default())
            .expect("Metal must construct an IOSurface texture");
        let mut resources = SessionResources::new();
        let presentations = executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateSurface(
                        7,
                        SurfaceDesc {
                            width: 4,
                            height: 3,
                            format: TextureFormat::Bgra8Unorm,
                            token: hl_gpu::SurfaceToken::new(9).unwrap(),
                        },
                    ),
                    Cmd::CreateTexture(8, texture(TextureFormat::Bgra8Unorm)),
                    Cmd::Present {
                        surface: 7,
                        texture: 8,
                        serial: hl_gpu::FrameSerial::new(11).unwrap(),
                    },
                ],
            )
            .expect("native presentation");
        assert_eq!(presentations.len(), 1);
        assert_eq!(
            executor.completion_wait_count(),
            0,
            "native presentation must not device-wide wait"
        );
        let image = executor
            .iosurface_image(&resources, presentations[0])
            .expect("native image lookup")
            .expect("native backing");
        assert!(
            !image.surface().handle().as_ptr().is_null(),
            "presentation image exposes a live borrowed native handle"
        );
        assert_ne!(
            executor
                .iosurface_id(&resources, 8)
                .expect("live texture lookup")
                .expect("native backing"),
            0
        );

        executor
            .execute(&mut resources, &[Cmd::DestroyTexture(8)])
            .expect("texture destruction");
        assert!(executor.iosurface_id(&resources, 8).is_err());
        assert!(
            image.completion().is_ready(),
            "the retained frame's explicit completion eventually observes prior GPU work"
        );
        assert_ne!(
            image.id, 0,
            "presentation lease survives texture destruction"
        );
        assert_eq!((image.width, image.height), (4, 3));
    }

    #[test]
    fn abandoned_and_failed_presentations_do_not_accumulate_completion_records() {
        let mut executor = WgpuExecutor::new_iosurface(DeviceConfig::default()).unwrap();
        let mut resources = SessionResources::new();
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateSurface(
                        7,
                        SurfaceDesc {
                            width: 4,
                            height: 3,
                            format: TextureFormat::Bgra8Unorm,
                            token: hl_gpu::SurfaceToken::new(9).unwrap(),
                        },
                    ),
                    Cmd::CreateTexture(8, texture(TextureFormat::Bgra8Unorm)),
                    Cmd::Present {
                        surface: 7,
                        texture: 8,
                        serial: hl_gpu::FrameSerial::new(11).unwrap(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(executor.presentation_completion_count(), 1);

        executor
            .execute(
                &mut resources,
                &[Cmd::Present {
                    surface: 7,
                    texture: 8,
                    serial: hl_gpu::FrameSerial::new(12).unwrap(),
                }],
            )
            .unwrap();
        assert_eq!(
            executor.presentation_completion_count(),
            1,
            "a newer result retires an abandoned older result for the same surface"
        );

        let failed = executor.execute(
            &mut resources,
            &[
                Cmd::Present {
                    surface: 7,
                    texture: 8,
                    serial: hl_gpu::FrameSerial::new(13).unwrap(),
                },
                Cmd::DestroyBuffer(99),
            ],
        );
        assert!(failed.is_err());
        assert_eq!(
            executor.presentation_completion_count(),
            1,
            "failed-batch completion is rolled back"
        );

        executor
            .execute(&mut resources, &[Cmd::DestroySurface(7)])
            .unwrap();
        assert_eq!(executor.presentation_completion_count(), 0);
    }
}
