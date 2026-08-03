//! Concrete GPU executor selection at the application composition boundary.

use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::{BufferId, Cmd, CpuExecutor, FenceId, GpuExecutor, Presentation, SessionResources};

use super::service::Backend;

pub(crate) enum Executor {
    Cpu(CpuExecutor),
    Wgpu(hl_gpu_wgpu::WgpuExecutor),
}

#[derive(Clone)]
pub(crate) enum Executors {
    Cpu,
    Wgpu(hl_gpu_wgpu::Device),
}

impl Executors {
    pub(crate) fn new(backend: Backend, native: bool) -> Result<Self, String> {
        match backend {
            Backend::Cpu => Ok(Self::Cpu),
            Backend::Wgpu => {
                #[cfg(target_os = "macos")]
                let device = if native {
                    hl_gpu_wgpu::Device::new_iosurface(Default::default())
                } else {
                    hl_gpu_wgpu::Device::new(Default::default())
                }
                .map_err(|error| error.to_string())?;
                #[cfg(not(target_os = "macos"))]
                let device = {
                    let _ = native;
                    hl_gpu_wgpu::Device::new(Default::default())
                        .map_err(|error| error.to_string())?
                };
                Ok(Self::Wgpu(device))
            }
        }
    }

    pub(crate) fn executor(&self) -> Executor {
        match self {
            Self::Cpu => {
                let mut executor = CpuExecutor::new();
                executor.set_kernel_compiler(KernelCompiler::compile);
                Executor::Cpu(executor)
            }
            Self::Wgpu(device) => {
                let mut executor = device.executor();
                executor.set_kernel_compiler(KernelCompiler::compile);
                Executor::Wgpu(executor)
            }
        }
    }
}

impl Executor {
    #[cfg(target_os = "macos")]
    pub(crate) fn image(
        &self,
        resources: &SessionResources,
        presentation: Presentation,
    ) -> hl_gpu::Result<Option<hl_gpu_wgpu::IoSurfaceImage>> {
        match self {
            Self::Cpu(_) => Ok(None),
            Self::Wgpu(executor) => executor.iosurface_image(resources, presentation),
        }
    }
}

impl GpuExecutor for Executor {
    fn capabilities(&self) -> hl_gpu::Capabilities {
        match self {
            Self::Cpu(executor) => executor.capabilities(),
            Self::Wgpu(executor) => executor.capabilities(),
        }
    }

    fn execute(
        &mut self,
        resources: &mut SessionResources,
        batch: &[Cmd],
    ) -> hl_gpu::Result<hl_gpu::Execution> {
        match self {
            Self::Cpu(executor) => executor.execute(resources, batch),
            Self::Wgpu(executor) => executor.execute(resources, batch),
        }
    }

    fn wait(
        &mut self,
        resources: &mut SessionResources,
        fence: FenceId,
        value: u64,
    ) -> hl_gpu::Result<()> {
        match self {
            Self::Cpu(executor) => executor.wait(resources, fence, value),
            Self::Wgpu(executor) => executor.wait(resources, fence, value),
        }
    }

    fn read_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
        offset: u64,
        len: usize,
    ) -> hl_gpu::Result<Vec<u8>> {
        match self {
            Self::Cpu(executor) => GpuExecutor::read_buffer(executor, resources, id, offset, len),
            Self::Wgpu(executor) => GpuExecutor::read_buffer(executor, resources, id, offset, len),
        }
    }

    fn export_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
    ) -> hl_gpu::Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        match self {
            Self::Cpu(executor) => GpuExecutor::export_buffer(executor, resources, id),
            Self::Wgpu(executor) => GpuExecutor::export_buffer(executor, resources, id),
        }
    }

    fn import_buffer(
        &self,
        resource: hl_gpu::runtime::model::sharing::Shared,
        bytes: u64,
    ) -> hl_gpu::Result<hl_gpu::runtime::model::resources::Native> {
        match self {
            Self::Cpu(executor) => GpuExecutor::import_buffer(executor, resource, bytes),
            Self::Wgpu(executor) => GpuExecutor::import_buffer(executor, resource, bytes),
        }
    }

    fn export_texture(&self, resources: &SessionResources, id: hl_gpu::TextureId) -> hl_gpu::Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        match self { Self::Cpu(executor) => GpuExecutor::export_texture(executor, resources, id), Self::Wgpu(executor) => GpuExecutor::export_texture(executor, resources, id) }
    }

    fn import_texture(&self, resource: hl_gpu::runtime::model::sharing::Shared, bytes: u64) -> hl_gpu::Result<hl_gpu::runtime::model::resources::Native> {
        match self { Self::Cpu(executor) => GpuExecutor::import_texture(executor, resource, bytes), Self::Wgpu(executor) => GpuExecutor::import_texture(executor, resource, bytes) }
    }

    fn sharing_barrier(&mut self) -> hl_gpu::Result<()> {
        match self {
            Self::Cpu(executor) => GpuExecutor::sharing_barrier(executor),
            Self::Wgpu(executor) => GpuExecutor::sharing_barrier(executor),
        }
    }
}

struct KernelCompiler;

impl KernelCompiler {
    fn compile(descriptor: &KernelDescriptor) -> hl_gpu::Result<KernelProgram> {
        hl_cuda::adapter::ptx::compile(&descriptor.ptx, &descriptor.entry, descriptor.block)
    }
}
