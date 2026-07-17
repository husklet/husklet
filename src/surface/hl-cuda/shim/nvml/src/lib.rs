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

    /// Serializes the two stateful NVML tests: they share the process-global seeded device state (whose
    /// `inited` flag `nvmlInit`/`nvmlShutdown` toggle), so they must never interleave under the parallel
    /// test runner. Both tests seed the SAME `HL_CUDA_*` env before init, so the once-only seed is
    /// deterministic regardless of which runs first.
    static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Seed identical device identity for both stateful tests (state is seeded once at the first init).
    fn seed_env() {
        std::env::set_var("HL_CUDA_CC", "7.5");
        std::env::set_var("HL_CUDA_VRAM_BYTES", (6u64 << 30).to_string());
    }

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
        let _serial = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Seed device identity BEFORE the first init (state is seeded once).
        seed_env();

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

    /// Exercises EVERY remaining `nvml*` entry point the first test does not touch — the full device-info
    /// surface — asserting each returns its real modeled value (or an honest `NVML_ERROR_NOT_SUPPORTED`
    /// for a genuinely hardware-specific query), never a fake success. Together with `init_enumerate_and_
    /// query` this drives all 62 exports.
    #[test]
    fn full_device_info_surface_answers_or_is_honestly_unsupported() {
        let _serial = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        seed_env();

        // init aliases + system version queries
        assert_eq!(nvmlInit(), 0);
        assert_eq!(nvmlInitWithFlags(0), 0);
        let mut drv = [0 as c_char; 96];
        assert_eq!(nvmlSystemGetDriverVersion(drv.as_mut_ptr(), 96), 0);
        assert!(!unsafe { std::ffi::CStr::from_ptr(drv.as_ptr()) }.to_bytes().is_empty());
        let mut nvv = [0 as c_char; 96];
        assert_eq!(nvmlSystemGetNVMLVersion(nvv.as_mut_ptr(), 96), 0);
        assert!(!unsafe { std::ffi::CStr::from_ptr(nvv.as_ptr()) }.to_bytes().is_empty());
        let mut cdv = -1i32;
        assert_eq!(nvmlSystemGetCudaDriverVersion(&mut cdv), 0);
        assert_eq!(cdv, 12020);
        cdv = -1;
        assert_eq!(nvmlSystemGetCudaDriverVersion_v2(&mut cdv), 0);
        assert_eq!(cdv, 12020);
        let mut pn = [0 as c_char; 64];
        assert_eq!(nvmlSystemGetProcessName(1234, pn.as_mut_ptr(), 64), 0);

        // handle acquisition variants all resolve the single device
        let mut dev: *mut c_void = core::ptr::null_mut();
        assert_eq!(nvmlDeviceGetHandleByIndex_v2(0, &mut dev), 0);
        assert!(!dev.is_null());
        let pci = std::ffi::CString::new("0000:00:00.0").unwrap();
        let mut dpci: *mut c_void = core::ptr::null_mut();
        assert_eq!(nvmlDeviceGetHandleByPciBusId(pci.as_ptr(), &mut dpci), 0);
        assert_eq!(nvmlDeviceGetHandleByPciBusId_v2(pci.as_ptr(), &mut dpci), 0);

        // UUID round-trip: read our device's UUID, then resolve a handle by it; a wrong UUID is NOT_FOUND.
        let mut uuid = [0 as c_char; 96];
        assert_eq!(nvmlDeviceGetUUID(dev, uuid.as_mut_ptr(), 96), 0);
        let uuid_s = unsafe { std::ffi::CStr::from_ptr(uuid.as_ptr()) }.to_string_lossy().into_owned();
        assert!(uuid_s.starts_with("GPU-"), "unexpected uuid: {uuid_s}");
        let uuid_c = std::ffi::CString::new(uuid_s).unwrap();
        let mut du: *mut c_void = core::ptr::null_mut();
        assert_eq!(nvmlDeviceGetHandleByUUID(uuid_c.as_ptr(), &mut du), 0);
        let wrong = std::ffi::CString::new("GPU-does-not-exist").unwrap();
        assert_eq!(nvmlDeviceGetHandleByUUID(wrong.as_ptr(), &mut du), 6 /* NOT_FOUND */);

        // identity
        let mut serial = [0 as c_char; 64];
        assert_eq!(nvmlDeviceGetSerial(dev, serial.as_mut_ptr(), 64), 0);
        assert!(unsafe { std::ffi::CStr::from_ptr(serial.as_ptr()) }.to_string_lossy().contains("HL-SIM"));
        let mut idx = 9u32;
        assert_eq!(nvmlDeviceGetIndex(dev, &mut idx), 0);
        assert_eq!(idx, 0);
        let mut minor = 9u32;
        assert_eq!(nvmlDeviceGetMinorNumber(dev, &mut minor), 0);
        assert_eq!(minor, 0);
        let mut brand = -1i32;
        assert_eq!(nvmlDeviceGetBrand(dev, &mut brand), 0);
        assert_eq!(brand, 3 /* NVML_BRAND_NVS */);
        let mut arch = 99u32;
        assert_eq!(nvmlDeviceGetArchitecture(dev, &mut arch), 0);
        assert_eq!(arch, 6, "cc 7.5 → TURING(6)");
        let mut vbios = [0 as c_char; 32];
        assert_eq!(nvmlDeviceGetVbiosVersion(dev, vbios.as_mut_ptr(), 32), 0);
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(vbios.as_ptr()) }.to_string_lossy(),
            "00.00.00.00.00"
        );

        // memory_v2 (version@0, total@8 in the #[repr(C)] layout) reports the seeded 6 GiB total.
        // Backed by a `[u64; 5]` (40 bytes, 8-aligned) so the struct's u64 fields are naturally aligned.
        let mut mem2 = [0u64; 5];
        assert_eq!(nvmlDeviceGetMemoryInfo_v2(dev, mem2.as_mut_ptr() as *mut c_void), 0);
        assert_eq!(mem2[1], 6u64 << 30, "total @ byte offset 8");

        // PCI info (all three versions share the body): the legacy bus id is field @0. Backed by a
        // `[u32; 24]` (96 bytes, 4-aligned) for the struct's u32 fields.
        for variant in [nvmlDeviceGetPciInfo as usize, nvmlDeviceGetPciInfo_v2 as usize, nvmlDeviceGetPciInfo_v3 as usize] {
            let f: extern "C" fn(*mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(variant) };
            let mut pinfo = [0u32; 24];
            assert_eq!(f(dev, pinfo.as_mut_ptr() as *mut c_void), 0);
            let legacy = unsafe { std::ffi::CStr::from_ptr(pinfo.as_ptr() as *const c_char) }.to_string_lossy();
            assert_eq!(legacy, "0000:00:00.0");
        }
        // The extended PCI struct is an undocumented private layout → honestly unsupported.
        let mut ext = [0u8; 128];
        assert_eq!(nvmlDeviceGetPciInfoExt(dev, ext.as_mut_ptr() as *mut c_void), 3 /* NOT_SUPPORTED */);

        // PCIe link + bus width + core count
        let mut g = 0u32;
        assert_eq!(nvmlDeviceGetCurrPcieLinkGeneration(dev, &mut g), 0);
        assert_eq!(g, 4);
        assert_eq!(nvmlDeviceGetMaxPcieLinkGeneration(dev, &mut g), 0);
        assert_eq!(g, 4);
        let mut w = 0u32;
        assert_eq!(nvmlDeviceGetCurrPcieLinkWidth(dev, &mut w), 0);
        assert_eq!(w, 16);
        assert_eq!(nvmlDeviceGetMaxPcieLinkWidth(dev, &mut w), 0);
        assert_eq!(w, 16);
        assert_eq!(nvmlDeviceGetMemoryBusWidth(dev, &mut w), 0);
        assert_eq!(w, 256);
        let mut cores = 0u32;
        assert_eq!(nvmlDeviceGetNumGpuCores(dev, &mut cores), 0);
        assert_eq!(cores, 4096);

        // power limits + max clocks + temperature threshold
        let mut mw = 0u32;
        assert_eq!(nvmlDeviceGetPowerManagementLimit(dev, &mut mw), 0);
        assert_eq!(mw, 70000);
        assert_eq!(nvmlDeviceGetEnforcedPowerLimit(dev, &mut mw), 0);
        assert_eq!(mw, 70000);
        let mut mhz = 0u32;
        assert_eq!(nvmlDeviceGetMaxClockInfo(dev, 2 /* MEM */, &mut mhz), 0);
        assert_eq!(mhz, 6000);
        assert_eq!(nvmlDeviceGetMaxClockInfo(dev, 0 /* GRAPHICS */, &mut mhz), 0);
        assert_eq!(mhz, 1500);
        let mut tt = 0u32;
        assert_eq!(nvmlDeviceGetTemperatureThreshold(dev, 0, &mut tt), 0);
        assert_eq!(tt, 90);

        // modes / states (all report a fixed idle value)
        let mut m = -1i32;
        assert_eq!(nvmlDeviceGetPersistenceMode(dev, &mut m), 0);
        assert_eq!(m, 0);
        assert_eq!(nvmlDeviceGetDisplayMode(dev, &mut m), 0);
        assert_eq!(m, 0);
        assert_eq!(nvmlDeviceGetComputeMode(dev, &mut m), 0);
        assert_eq!(m, 0);
        assert_eq!(nvmlDeviceGetPerformanceState(dev, &mut m), 0);
        assert_eq!(m, 0);
        assert_eq!(nvmlDeviceGetPowerState(dev, &mut m), 0);
        assert_eq!(m, 0);

        // encoder/decoder utilization
        let (mut u, mut samp) = (9u32, 0u32);
        assert_eq!(nvmlDeviceGetEncoderUtilization(dev, &mut u, &mut samp), 0);
        assert_eq!((u, samp), (0, 167000));
        u = 9;
        assert_eq!(nvmlDeviceGetDecoderUtilization(dev, &mut u, &mut samp), 0);
        assert_eq!((u, samp), (0, 167000));

        // running-process queries (all versions): none running
        let mut c = 42u32;
        for variant in [
            nvmlDeviceGetComputeRunningProcesses_v2 as usize,
            nvmlDeviceGetComputeRunningProcesses_v3 as usize,
            nvmlDeviceGetGraphicsRunningProcesses as usize,
            nvmlDeviceGetGraphicsRunningProcesses_v2 as usize,
            nvmlDeviceGetGraphicsRunningProcesses_v3 as usize,
        ] {
            let f: extern "C" fn(*mut c_void, *mut u32, *mut c_void) -> i32 = unsafe { core::mem::transmute(variant) };
            c = 42;
            assert_eq!(f(dev, &mut c, core::ptr::null_mut()), 0);
            assert_eq!(c, 0);
        }

        // A bogus device handle is honestly rejected (INVALID_ARGUMENT), never a fabricated answer.
        let bogus = 0x9999usize as *mut c_void;
        assert_eq!(nvmlDeviceGetSerial(bogus, serial.as_mut_ptr(), 64), 2 /* INVALID_ARGUMENT */);
        assert_eq!(nvmlDeviceGetNumGpuCores(bogus, &mut cores), 2);

        assert_eq!(nvmlShutdown(), 0);
    }
}
