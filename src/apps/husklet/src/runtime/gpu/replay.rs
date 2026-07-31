//! Deterministic headless replay of application-owned GPU captures.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hl_gpu::protocol::model::descriptor::{BufferDesc, TextureDesc};
use hl_gpu::protocol::model::enums::{buffer_usage, TextureFormat};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, GlobalLedger, GpuExecutor, Limits, Session, SystemClock,
};

use super::capture;

#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub pixels: PathBuf,
}

pub struct Replay;

impl Replay {
    pub fn run(capture_path: &Path, output: &Path) -> io::Result<Vec<Frame>> {
        let total_started = Instant::now();
        let batches = capture::Trace::read(capture_path)?;
        if output.exists() && fs::read_dir(output)?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "GPU replay output directory is not empty",
            ));
        }
        fs::create_dir_all(output)?;
        let device_started = Instant::now();
        let device = hl_gpu_wgpu::Device::new(Default::default()).map_err(io::Error::other)?;
        let device_elapsed = device_started.elapsed();
        let mut executor = device.executor();
        executor.enable_profile();
        let mut session = Session::new(
            Limits::from_capabilities(executor.capabilities()),
            GlobalLedger::unbounded(),
            Box::new(SystemClock::new()),
        );
        let mut textures = BTreeMap::new();
        let mut buffers = BTreeSet::new();
        let mut frames = Vec::new();
        let mut stats = Stats::default();

        for batch in batches {
            stats.record(&batch);
            let encoded_bytes = hl_gpu::Encoder::stream(&batch).len();
            let submit_started = Instant::now();
            let presentations =
                hl_gpu::runtime::submit(&mut session, &mut executor, encoded_bytes, &batch)
                    .map_err(io::Error::other)?;
            stats.submit_elapsed += submit_started.elapsed();
            let presented = State::apply(&mut textures, &mut buffers, &batch)?;
            if presentations.len() != presented.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "replay presentation count differs from captured commands",
                ));
            }
            for (presentation, descriptor) in presentations.into_iter().zip(presented) {
                let temporary = State::available_buffer(&buffers)?;
                let bytes_per_texel = descriptor.format.bytes_per_texel().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "presented texture format has no packed pixel representation",
                    )
                })? as u32;
                let bytes_per_row =
                    descriptor
                        .width
                        .checked_mul(bytes_per_texel)
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "frame row is too large")
                        })?;
                let length = u64::from(bytes_per_row)
                    .checked_mul(u64::from(descriptor.height))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "frame is too large")
                    })?;
                let diagnostic = [
                    Cmd::CreateBuffer(
                        temporary,
                        BufferDesc {
                            size: length,
                            usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                            label: "husklet-replay-frame".to_owned(),
                        },
                    ),
                    Cmd::Submit(CommandBuffer {
                        encoder: vec![Enc::CopyTextureToBuffer {
                            src: presentation.texture.0,
                            mip: 0,
                            width: descriptor.width,
                            height: descriptor.height,
                            dst: temporary,
                            dst_offset: 0,
                            bytes_per_row,
                        }],
                        signal: None,
                    }),
                ];
                let encoded_bytes = hl_gpu::Encoder::stream(&diagnostic).len();
                hl_gpu::runtime::submit(&mut session, &mut executor, encoded_bytes, &diagnostic)
                    .map_err(io::Error::other)?;
                let read_started = Instant::now();
                let pixels = executor
                    .read_buffer(
                        &session.resources,
                        BufferId(temporary),
                        0,
                        usize::try_from(length).map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "frame does not fit host memory",
                            )
                        })?,
                    )
                    .map_err(io::Error::other)?;
                stats.read_elapsed += read_started.elapsed();
                let index = frames.len();
                let pixel_path = output.join(format!("frame-{index:04}.raw"));
                fs::write(&pixel_path, &pixels)?;
                let metadata = format!(
                "serial={}\nwidth={}\nheight={}\nformat={:?}\nbytes_per_row={bytes_per_row}\nbytes={}\n",
                presentation.serial.get(),
                descriptor.width,
                descriptor.height,
                descriptor.format,
                pixels.len()
            );
                fs::write(output.join(format!("frame-{index:04}.txt")), metadata)?;
                let destroy = [Cmd::DestroyBuffer(temporary)];
                hl_gpu::runtime::submit(
                    &mut session,
                    &mut executor,
                    hl_gpu::Encoder::stream(&destroy).len(),
                    &destroy,
                )
                .map_err(io::Error::other)?;
                frames.push(Frame {
                    serial: presentation.serial.get(),
                    width: descriptor.width,
                    height: descriptor.height,
                    format: descriptor.format,
                    pixels: pixel_path,
                });
            }
        }
        if frames.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GPU capture contains no presentation",
            ));
        }
        if let Some(profile) = executor.profile() {
            stats.print(device_elapsed, total_started.elapsed(), &profile);
        }
        Ok(frames)
    }
}

#[derive(Default)]
struct Stats {
    batches: u64,
    commands: BTreeMap<&'static str, u64>,
    encoder_ops: u64,
    submit_elapsed: Duration,
    read_elapsed: Duration,
}

impl Stats {
    fn record(&mut self, batch: &[Cmd]) {
        self.batches += 1;
        for command in batch {
            let name = match command {
                Cmd::CreateBuffer(..) => "create_buffer",
                Cmd::DestroyBuffer(..) => "destroy_buffer",
                Cmd::WriteBuffer { .. } => "write_buffer",
                Cmd::CreateTexture(..) => "create_texture",
                Cmd::DestroyTexture(..) => "destroy_texture",
                Cmd::CreateTextureView(..) => "create_texture_view",
                Cmd::DestroyTextureView(..) => "destroy_texture_view",
                Cmd::CreateSampler(..) => "create_sampler",
                Cmd::DestroySampler(..) => "destroy_sampler",
                Cmd::CreateShader { .. } => "create_shader",
                Cmd::DestroyShader(..) => "destroy_shader",
                Cmd::CreateRenderPipelineLayout(..) => "create_render_pipeline_layout",
                Cmd::CreateRenderPipeline(..) => "create_render_pipeline",
                Cmd::CreateComputePipelineLayout(..) => "create_compute_pipeline_layout",
                Cmd::CreateComputePipeline(..) => "create_compute_pipeline",
                Cmd::DestroyPipeline(..) => "destroy_pipeline",
                Cmd::CreateBindGroup(..) => "create_bind_group",
                Cmd::DestroyBindGroup(..) => "destroy_bind_group",
                Cmd::CreateSurface(..) => "create_surface",
                Cmd::DestroySurface(..) => "destroy_surface",
                Cmd::CreateFence(..) => "create_fence",
                Cmd::DestroyFence(..) => "destroy_fence",
                Cmd::Submit(buffer) => {
                    self.encoder_ops += buffer.encoder.len() as u64;
                    "submit"
                }
                Cmd::WaitFence { .. } => "wait_fence",
                Cmd::Present { .. } => "present",
            };
            *self.commands.entry(name).or_default() += 1;
        }
    }

    fn print(&self, device: Duration, total: Duration, profile: &hl_gpu_wgpu::Profile) {
        eprintln!(
            "gpu replay profile: batches={} commands={} encoder_ops={} device={device:?} submit={:?} readback={:?} total={total:?}",
            self.batches,
            self.commands.values().sum::<u64>(),
            self.encoder_ops,
            self.submit_elapsed,
            self.read_elapsed,
        );
        for (name, count) in &self.commands {
            eprintln!("  command {name}: {count}");
        }
        eprintln!(
            "  shaders: {} {:?}; render_pipelines: {} {:?} ({} native compilations); compute_pipelines: {} {:?}; bind_groups: {} {:?}",
            profile.shaders.count,
            profile.shaders.elapsed,
            profile.render_pipelines.count,
            profile.render_pipelines.elapsed,
            profile.render_pipeline_compilations,
            profile.compute_pipelines.count,
            profile.compute_pipelines.elapsed,
            profile.bind_groups.count,
            profile.bind_groups.elapsed,
        );
        eprintln!(
            "  logical_submissions: {} {:?}; render_passes: {} {:?}; compute_passes: {} {:?}; native_submissions: {}; waits: {} {:?}",
            profile.logical_submissions.count,
            profile.logical_submissions.elapsed,
            profile.render_passes.count,
            profile.render_passes.elapsed,
            profile.compute_passes.count,
            profile.compute_passes.elapsed,
            profile.native_submissions,
            profile.waits.count,
            profile.waits.elapsed,
        );
    }
}

struct State;

impl State {
    fn apply(
        textures: &mut BTreeMap<u32, TextureDesc>,
        buffers: &mut BTreeSet<u32>,
        batch: &[Cmd],
    ) -> io::Result<Vec<TextureDesc>> {
        let mut presented = Vec::new();
        for command in batch {
            match command {
                Cmd::CreateTexture(id, descriptor) => {
                    textures.insert(*id, descriptor.clone());
                }
                Cmd::DestroyTexture(id) => {
                    textures.remove(id);
                }
                Cmd::CreateBuffer(id, _) => {
                    buffers.insert(*id);
                }
                Cmd::DestroyBuffer(id) => {
                    buffers.remove(id);
                }
                Cmd::Present { texture, .. } => {
                    presented.push(textures.get(texture).cloned().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("capture presents missing texture {texture}"),
                        )
                    })?);
                }
                _ => {}
            }
        }
        Ok(presented)
    }

    fn available_buffer(buffers: &BTreeSet<u32>) -> io::Result<u32> {
        (1..u32::MAX)
            .find(|id| !buffers.contains(id))
            .ok_or_else(|| io::Error::other("no GPU buffer id is available for frame extraction"))
    }
}

#[cfg(test)]
mod tests {
    use hl_gpu::protocol::model::descriptor::{SurfaceDesc, TextureDesc};
    use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
    use hl_gpu::{Cmd, CommandBuffer, Enc, FrameSerial, SurfaceToken};

    use super::*;

    #[test]
    fn replay_writes_exact_presented_pixels() {
        if hl_gpu_wgpu::Device::new(Default::default()).is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let capture_path = root.path().join("frame.hgpu");
        let output = root.path().join("output");
        let batch = vec![
            Cmd::CreateSurface(
                1,
                SurfaceDesc {
                    width: 2,
                    height: 1,
                    format: TextureFormat::Rgba8Unorm,
                    token: SurfaceToken::new(1).unwrap(),
                },
            ),
            Cmd::CreateTexture(
                2,
                TextureDesc {
                    width: 2,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::COPY_DST
                        | texture_usage::COPY_SRC
                        | texture_usage::PRESENT,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ClearRect {
                    texture: 2,
                    x: 0,
                    y: 0,
                    w: 2,
                    h: 1,
                    color: [1.0, 0.0, 0.0, 1.0],
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                }],
                signal: None,
            }),
            Cmd::Present {
                surface: 1,
                texture: 2,
                serial: FrameSerial::new(7).unwrap(),
            },
        ];
        capture::Trace::write(&capture_path, &[batch]).unwrap();

        let frames = Replay::run(&capture_path, &output).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].serial, 7);
        assert_eq!(
            fs::read(&frames[0].pixels).unwrap(),
            [255, 0, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn replay_rejects_missing_resource_dependencies() {
        if hl_gpu_wgpu::Device::new(Default::default()).is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let capture_path = root.path().join("missing.hgpu");
        capture::Trace::write(
            &capture_path,
            &[vec![Cmd::Present {
                surface: 1,
                texture: 2,
                serial: FrameSerial::new(1).unwrap(),
            }]],
        )
        .unwrap();

        let error = Replay::run(&capture_path, &root.path().join("output")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }
}
