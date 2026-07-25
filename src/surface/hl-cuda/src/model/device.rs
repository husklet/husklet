//! The simulated CUDA **device** — the numbers NVML / `nvidia-smi` / the CUDA runtime report so a
//! probe (`torch.cuda.is_available()`, `cuDeviceGetAttribute`, `nvidia-smi -L`) accepts the device —
//! plus the opaque device-pointer handle the driver API hands back.
//!
//! Ported from `hl-gpu/src/cuda.rs` (`CudaDeviceDesc` / `DevicePtr`). Pure data + formatting; the values
//! present as a plausible mid-range Ampere card while actually being served by the host GPU.

/// A CUDA device pointer (opaque `CUdeviceptr` value handed back to the guest). It is a flat address in
/// the driver's simulated unified VA; [`super::memory::Allocations`] maps it back to a backing buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct DevicePtr(pub u64);

/// What NVML / `nvidia-smi` / `cudaGetDeviceProperties` report for the simulated device.
#[derive(Clone, PartialEq, Debug)]
pub struct CudaDeviceDesc {
    pub name: String,
    /// (major, minor) compute capability, e.g. (8, 6) ~ Ampere.
    pub compute_capability: (u32, u32),
    /// Reported VRAM in bytes (on Apple Silicon this is carved from unified memory).
    pub total_mem: u64,
    pub multiprocessor_count: u32,
    pub warp_size: u32,
    pub max_threads_per_block: u32,
    pub clock_khz: u32,
    /// 16-byte GPU UUID reported by NVML (`GPU-xxxxxxxx-....`).
    pub uuid: [u8; 16],
    pub pci_bus_id: String,
}

impl CudaDeviceDesc {
    /// A sensible default for an Apple-silicon host: presents as a mid-range Ampere-class device backed
    /// by unified memory. `vram_bytes` should be a slice of the machine's RAM the user allows.
    pub fn apple_default(vram_bytes: u64) -> Self {
        Self {
            name: "hl Metal (CUDA-sim) Device".into(),
            compute_capability: (8, 6),
            total_mem: vram_bytes,
            multiprocessor_count: 32,
            warp_size: 32,
            max_threads_per_block: 1024,
            clock_khz: 1_500_000,
            uuid: *b"hl-metal-cuda\0\0\0",
            pci_bus_id: "0000:00:00.0".into(),
        }
    }

    pub fn compute_capability_str(&self) -> String {
        format!(
            "{}.{}",
            self.compute_capability.0, self.compute_capability.1
        )
    }

    /// A UUID in NVML's textual `GPU-...` form.
    pub fn uuid_str(&self) -> String {
        let h: String = self.uuid.iter().map(|b| format!("{:02x}", b)).collect();
        format!(
            "GPU-{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    }

    /// The line `nvidia-smi -L` would print for this device (index `idx`).
    pub fn nvidia_smi_l_line(&self, idx: u32) -> String {
        format!("GPU {}: {} (UUID: {})", idx, self.name, self.uuid_str())
    }
}
