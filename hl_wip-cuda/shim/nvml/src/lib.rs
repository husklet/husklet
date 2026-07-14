//! Guest cdylib deployed as `libnvidia-ml.so.1` — the NVML drop-in.
//!
//! The exported `nvml*` surface is code-generated from `registry/nvml.manifest` (`build.rs`), extracted
//! from the clean-room oracle `hl-gpu/nvml/nvml_shim.c`, so it can never drift from that 62-entry surface.
//! The init/shutdown/error/count/handle/name/version basics have real bodies in [`nvml`] so a probe
//! enumerates the single simulated device; the rest are benign `NVML_SUCCESS` stubs ([`stub`]). The
//! soname `libnvidia-ml.so.1` is baked by `build.rs`.

#![allow(non_snake_case)]

pub mod nvml;
pub mod stub;

// The generated C-ABI export surface: every `nvml*` entry point not hand-written in `nvml`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported NVML entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = NVML_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::nvml::*;
    use super::*;
    use core::ffi::{c_char, c_void};

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(NVML_ENTRYPOINTS, 62, "NVML surface drifted from the oracle's 62 exports");
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
        // The whole surface has real hand-written bodies — no generated default stubs remain.
        assert_eq!(GENERATED_STUBS, 0, "nvml still has default stubs");
    }

    // A single serial test drives init → enumerate → identity/memory/sensors/clocks, so the seeded
    // process-global device state (init toggles `inited`) is never raced across parallel tests.
    #[test]
    fn init_enumerate_and_query() {
        // Seed device identity BEFORE the first init (state is seeded once).
        std::env::set_var("HL_CUDA_CC", "7.5");
        std::env::set_var("HL_CUDA_VRAM_BYTES", (6u64 << 30).to_string());

        // Before init, enumeration reports NVML_ERROR_UNINITIALIZED (1).
        let mut count = 0u32;
        assert_eq!(nvmlDeviceGetCount(&mut count), 1);

        assert_eq!(nvmlInit_v2(), 0);
        assert_eq!(nvmlDeviceGetCount_v2(&mut count), 0);
        assert_eq!(count, 1);

        let mut dev: *mut c_void = core::ptr::null_mut();
        assert_eq!(nvmlDeviceGetHandleByIndex(0, &mut dev), 0);
        assert!(!dev.is_null());
        assert_eq!(nvmlDeviceGetHandleByIndex(1, &mut dev), 2); // only one device → INVALID_ARGUMENT

        // re-fetch the valid handle
        assert_eq!(nvmlDeviceGetHandleByIndex(0, &mut dev), 0);

        // name
        let mut name = [0 as c_char; 96];
        assert_eq!(nvmlDeviceGetName(dev, name.as_mut_ptr(), 96), 0);
        let nm = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
        assert!(nm.contains("CUDA-sim"), "unexpected name: {nm}");

        // compute capability from HL_CUDA_CC=7.5
        let (mut maj, mut min) = (-1i32, -1i32);
        assert_eq!(nvmlDeviceGetCudaComputeCapability(dev, &mut maj, &mut min), 0);
        assert_eq!((maj, min), (7, 5));

        // memory total from HL_CUDA_VRAM_BYTES (read `total` at offset 0 of nvmlMemory_t)
        let mut mem = [0u8; 32];
        assert_eq!(nvmlDeviceGetMemoryInfo(dev, mem.as_mut_ptr() as *mut c_void), 0);
        let total = u64::from_le_bytes(mem[0..8].try_into().unwrap());
        assert_eq!(total, 6u64 << 30);

        // utilization (read `gpu` at offset 0 of nvmlUtilization_t)
        let mut util = [0u8; 8];
        assert_eq!(nvmlDeviceGetUtilizationRates(dev, util.as_mut_ptr() as *mut c_void), 0);
        assert_eq!(u32::from_le_bytes(util[0..4].try_into().unwrap()), 0);

        // temperature + clocks (modeled sane values)
        let mut temp = 0u32;
        assert_eq!(nvmlDeviceGetTemperature(dev, 0, &mut temp), 0);
        assert_eq!(temp, 35);
        let mut mhz = 0u32;
        assert_eq!(nvmlDeviceGetClockInfo(dev, 2 /* MEM */, &mut mhz), 0);
        assert_eq!(mhz, 6000);
        assert_eq!(nvmlDeviceGetClockInfo(dev, 0 /* GRAPHICS */, &mut mhz), 0);
        assert_eq!(mhz, 1500);

        // power
        let mut mw = 0u32;
        assert_eq!(nvmlDeviceGetPowerUsage(dev, &mut mw), 0);
        assert_eq!(mw, 25000);

        // running processes: none
        let mut procs = 42u32;
        assert_eq!(
            nvmlDeviceGetComputeRunningProcesses(dev, &mut procs, core::ptr::null_mut()),
            0
        );
        assert_eq!(procs, 0);

        // genuinely hardware-specific queries are honestly NOT_SUPPORTED (3), never a fake value
        let mut fan = 0u32;
        assert_eq!(nvmlDeviceGetFanSpeed(dev, &mut fan), 3);
        let (mut cur, mut pend) = (0u32, 0u32);
        assert_eq!(nvmlDeviceGetMigMode(dev, &mut cur, &mut pend), 3);

        // error string
        let s = unsafe { std::ffi::CStr::from_ptr(nvmlErrorString(2)) }.to_string_lossy();
        assert_eq!(s, "A supplied argument is invalid");

        // private export-table handshake hands back a non-null table
        let mut table: *const c_void = core::ptr::null();
        assert_eq!(nvmlInternalGetExportTable(&mut table, core::ptr::null_mut()), 0);
        assert!(!table.is_null());

        assert_eq!(nvmlShutdown(), 0);
    }
}
