//! wgpu instance / adapter / device acquisition — the one place platform selection happens.
//!
//! Headless by construction: no window, no surface. On this Linux host the loader is pointed at the
//! software Vulkan ICD (lavapipe / `llvmpipe`) so the whole conformance suite runs with no GPU and no X
//! display; the same code selects a real Vulkan/Metal/DX adapter on a workstation. The adapter is
//! requested with `compatible_surface: None` (surfaceless) and `force_fallback_adapter` left to the
//! caller's [`DeviceConfig`].

use hl_gpu::{GpuError, Result};
use hl_log::tag;

/// How the executor picks its wgpu adapter/device. Defaults are headless-software friendly.
#[derive(Clone, Debug)]
pub struct DeviceConfig {
    /// wgpu backend bitset to consider. Defaults to `VULKAN` so the loader binds the lavapipe ICD on this
    /// Linux host (rather than the GL software rasterizer, which wgpu would otherwise prefer). A Metal/DX
    /// host overrides this with its own backend.
    pub backends: wgpu::Backends,
    /// Ask wgpu for its software fallback adapter. Off by default: with the loader pointed at lavapipe the
    /// only Vulkan adapter *is* software, so the normal high-performance request already lands on it.
    pub force_fallback: bool,
    /// If set, force the Vulkan loader to this ICD manifest before enumerating adapters (the lavapipe
    /// headless path). Applied process-wide via `VK_ICD_FILENAMES`.
    pub vk_icd_filenames: Option<String>,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        // Platform-selected backend: macOS has Metal (no Vulkan ICD), Linux/others bind the software
        // Vulkan ICD (lavapipe) so the headless conformance suite runs with no GPU + no display.
        #[cfg(target_os = "macos")]
        {
            Self {
                backends: wgpu::Backends::METAL,
                force_fallback: false,
                vk_icd_filenames: None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                backends: wgpu::Backends::VULKAN,
                force_fallback: false,
                // The lavapipe manifest shipped on this Linux host; harmless if absent (the loader
                // ignores an override pointing at a missing manifest and falls back to system ICDs).
                vk_icd_filenames: Some("/usr/share/vulkan/icd.d/lvp_icd.json".to_string()),
            }
        }
    }
}

/// A live wgpu device + queue and the human-readable adapter name (e.g. `"llvmpipe (LLVM ...)"`), plus
/// the [`wgpu::AdapterInfo`] so the executor can report what it actually bound.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
}

/// Acquire a headless (surfaceless) wgpu device per `cfg`. Blocks on the async requests with pollster.
pub fn acquire(cfg: &DeviceConfig) -> Result<Gpu> {
    if let Some(icd) = &cfg.vk_icd_filenames {
        // Point the Vulkan loader at the software ICD before the instance enumerates drivers. Safe: done
        // at device bring-up, before any wgpu/Vulkan call in this process.
        std::env::set_var("VK_ICD_FILENAMES", icd);
    }

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: cfg.backends,
        ..Default::default()
    });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        force_fallback_adapter: cfg.force_fallback,
        compatible_surface: None, // headless: no swapchain surface
    }))
    .ok_or(GpuError::Unsupported("wgpu: no adapter (is a Vulkan ICD / lavapipe reachable?)"))?;

    let info = adapter.get_info();
    hl_log::hl_info!(
        tag::WGPU,
        "adapter={} backend={:?} type={:?}",
        info.name,
        info.backend,
        info.device_type
    );

    // Enable PUSH_CONSTANTS when the adapter really supports it (lavapipe does). A guest wgpu (e.g. Zed's
    // gpui_wgpu) builds an internal indirect-draw-validation compute pipeline during device creation whose
    // shader declares `var<push_constant>`. That SPIR-V reaches this executor's `Device::create_shader_module`,
    // and naga's validator only enables the `PUSH_CONSTANT` capability when the device requested the
    // PUSH_CONSTANTS feature — otherwise it rejects the module ("Capability PUSH_CONSTANT is not supported"),
    // which the guest sees as a lost device. Requesting the feature only when advertised keeps this truthful
    // on backends that lack it. `adapter.limits()` already carries the adapter's real `max_push_constant_size`,
    // which the feature requires to be nonzero.
    let required_features = adapter.features() & wgpu::Features::PUSH_CONSTANTS;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("hl-gpu-wgpu"),
            required_features,
            // Downlevel/software-friendly: lavapipe advertises modest limits, so request the adapter's own
            // limits rather than the desktop defaults (which a software device may not meet).
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .map_err(|e| GpuError::Kernel(format!("wgpu: request_device failed: {e}")))?;

    Ok(Gpu { device, queue, info })
}
