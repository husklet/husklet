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
    /// The `(lowest, highest)` compute capability this device may report.
    ///
    /// **What the reported capability means here, and why it is not the IR's real feature level.**
    ///
    /// The kernel IR the PTX front-end lowers onto is far smaller than any real compute capability. As of
    /// today it covers: 32-bit integer ALU (`mov`/`add`/`sub`/`mul.lo`/`mul.wide.[su]32`/`mad.lo`/shifts/
    /// bitwise), f32 `add`/`sub`/`mul`/`fma`, integer `setp` AND the IEEE-754 f32 `setp` in both PTX
    /// families (ordered `lt`/`le`/`gt`/`ge`/`eq`/`ne`, unordered `ltu`/…/`neu`), `cvt` between f32 and
    /// s32/u32 in both integer rounding modes (`.rzi` truncating, `.rni` nearest-ties-to-even) plus
    /// `s64<-s32` and same-width integer reinterpretation, `ld`/`st` in `.global` and `.shared` through a
    /// register address, integer `atom`/`red` including `cas`, `bar.sync`, `membar`/`fence` at
    /// `cta`/`gl`/`sys` scope, and `bra` predicated on a `setp` result. It has NO f64/f16/bf16, no warp
    /// intrinsics (`vote`/`shfl`/`match`/`%laneid`/`%warpid`), no dynamic (`extern`) shared memory, no
    /// module-scope `.global` variables, no `atom.inc`/`dec`, no `mad.rn.f32` (only the fused `fma`), no
    /// `setp.num`/`setp.nan` and no fused-predicate `setp`, and no floor/ceil (`.rmi`/`.rpi`) conversion.
    ///
    /// Measured against the MANDATORY feature set of each capability level, the highest one this fully
    /// covers is **1.1** — unchanged by the f32 compare, conversion and fence work above: 1.2 already
    /// requires warp `vote` and 1.3 requires f64, neither of which exists here. So there is still no usable
    /// capability that is also a truthful feature claim.
    ///
    /// Clamping to 1.1 was rejected. It would make every application fail, including the ones that
    /// compute correct results today: CUDA 12.2 (the version `cuDriverGetVersion` reports) has no PTX ISA
    /// or `nvcc` target below 5.0, so a 1.1 device is refused by the toolchain before a single kernel is
    /// examined. It would also contradict the rest of the device table — 1024 threads/block, 48 KiB shared
    /// memory per block, unified addressing, managed memory — which no 1.1 part had.
    ///
    /// The decision is therefore: **the reported capability is a toolchain-compatibility contract, not a
    /// feature claim.** It says which PTX/cubin variant an application should hand us, and the default
    /// (8, 6) is a variant CUDA 12.2 emits. The actual feature gap is enforced where it is observable and
    /// loud — the PTX front-end rejects every construct listed above, so `cuModuleLoadData`/`cuLaunchKernel`
    /// return `CUDA_ERROR_INVALID_PTX` / `CUDA_ERROR_NOT_SUPPORTED` instead of a wrong number, and the
    /// capability-derived attributes that would be lies are already reported as absent
    /// (`CU_DEVICE_ATTRIBUTE_COOPERATIVE_LAUNCH` = 0, `MAX_DYNAMIC_SHARED_SIZE_BYTES` = 0). A
    /// capability-branching library that picks an sm_86 path it cannot be given fails at load, visibly.
    ///
    /// What this constant adds: the configured value must be one CUDA 12.2 can actually target. `5.0` is
    /// the oldest architecture that toolchain supports and `9.0` (Hopper) the newest it emits, so an
    /// `HL_CUDA_CC` outside that range is refused and the default is kept — rather than advertising a
    /// capability for which no application could produce a kernel at all.
    ///
    /// The Ampere-class SM budget the occupancy math uses (2048 threads, 65536 registers and 102400
    /// shared bytes per SM, 32 blocks per SM) deliberately stays fixed rather than tracking this value:
    /// those are the numbers `cuDeviceGetAttribute`/`cudaGetDeviceProperties` report for the modeled
    /// device, and deriving them from a configurable capability would only let occupancy disagree with the
    /// attributes an application reads.
    pub const SUPPORTED_CAPABILITY: ((u32, u32), (u32, u32)) = ((5, 0), (9, 0));

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

    /// Apply the product-configured device identity (`HL_CUDA_NAME`, `HL_CUDA_CC` as `"major.minor"`).
    /// Empty / unparsable input leaves the corresponding field at its default.
    ///
    /// All three guest libraries must apply this: `libcuda`/`libcudart` reporting a hardcoded name and
    /// compute capability while `libnvidia-ml` reports the configured one makes the device
    /// self-contradictory to any application that reads both (`nvidia-smi` versus
    /// `cudaGetDeviceProperties`).
    pub fn configure(&mut self, name: Option<&str>, compute_capability: Option<&str>) {
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            self.name = name.to_owned();
        }
        if let Some(capability) = compute_capability.and_then(Self::capability) {
            self.compute_capability = capability;
        }
    }

    /// Parse a `"major.minor"` compute capability (a bare `"8"` means `(8, 0)`), rejecting anything the
    /// advertised driver could not honour — see [`Self::SUPPORTED_CAPABILITY`].
    fn capability(text: &str) -> Option<(u32, u32)> {
        let mut parts = text.trim().split('.');
        let major = parts.next()?.trim().parse::<u32>().ok()?;
        let minor = parts
            .next()
            .and_then(|m| m.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let (low, high) = Self::SUPPORTED_CAPABILITY;
        ((low..=high).contains(&(major, minor))).then_some((major, minor))
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

#[cfg(test)]
mod tests {
    use super::CudaDeviceDesc;

    /// `HL_CUDA_CC` is a toolchain-compatibility contract (see
    /// [`CudaDeviceDesc::SUPPORTED_CAPABILITY`]): a value CUDA 12.2 can target is honoured, and one it
    /// cannot — the honest sm_11-class feature level of the kernel IR, or a made-up future one — is
    /// refused so the device never advertises a capability no application could compile for.
    #[test]
    fn a_configured_capability_outside_the_cuda_12_range_is_refused() {
        let mut device = CudaDeviceDesc::apple_default(1 << 30);
        let default = device.compute_capability;
        assert_eq!(default, (8, 6));

        // Accepted: the range CUDA 12.2 emits, ends included.
        for accepted in ["5.0", "7.5", "8.9", "9.0"] {
            let mut d = CudaDeviceDesc::apple_default(1 << 30);
            d.configure(None, Some(accepted));
            let (major, minor) = d.compute_capability;
            assert_eq!(format!("{major}.{minor}"), accepted);
        }

        // Refused, default kept: below the toolchain floor (including the IR's real 1.1 feature level),
        // above what CUDA 12.2 emits, and unparsable text.
        for refused in ["1.1", "2.0", "3.5", "4.9", "9.1", "99.99", "sm_86", ""] {
            device.configure(None, Some(refused));
            assert_eq!(
                device.compute_capability, default,
                "`{refused}` must not be reported as a compute capability"
            );
        }
    }
}
