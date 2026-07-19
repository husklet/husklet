//! The hand-written `nvml*` entry points — the whole 62-entry surface, at parity with hl's clean-room
//! NVML oracle `hl-gpu/nvml/nvml_shim.c`. NVML is a device-*info* surface (no compute path, no command
//! sink): it answers the numbers an `nvidia-smi`-style enumeration expects for hl's single simulated
//! device. Device identity (name / compute-capability / VRAM / versions) is seeded once at `nvmlInit`
//! from the `HL_CUDA_*` env the launcher sets; unmodeled/hardware-specific queries return an honest
//! `NVML_ERROR_NOT_SUPPORTED` (so `nvidia-smi` degrades to "N/A") rather than a fake value.

use core::ffi::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

use hl_cuda::CudaDeviceDesc;

// ---- nvmlReturn_t values (stable NVML ABI; from nvml_min.h) --------------------------------------
const NVML_SUCCESS: i32 = 0;
const NVML_ERROR_UNINITIALIZED: i32 = 1;
const NVML_ERROR_INVALID_ARGUMENT: i32 = 2;
const NVML_ERROR_NOT_SUPPORTED: i32 = 3;
const NVML_ERROR_NOT_FOUND: i32 = 6;

// ---- NVML enum values referenced by the modeled answers (from nvml_min.h) ------------------------
const NVML_CLOCK_MEM: i32 = 2;
const NVML_COMPUTEMODE_DEFAULT: i32 = 0;
const NVML_FEATURE_DISABLED: i32 = 0;
const NVML_BRAND_NVS: i32 = 3;
const NVML_PSTATE_0: i32 = 0;

/// A single, stable, non-null opaque `nvmlDevice_t` for the one simulated device. The handle's contents
/// are never dereferenced; only its identity is checked ([`is_valid`]).
const DEVICE_TOKEN: usize = 1;

// ==================================================================================================
// seeded device state (single device) — mirrors the C oracle's globals
// ==================================================================================================

/// Everything NVML reports for hl's one fabricated device, seeded once at `nvmlInit` from `HL_CUDA_*`.
struct Nvml {
    inited: bool,
    desc: CudaDeviceDesc,
    serial: String,
    driver_version: String,
    nvml_version: String,
    /// CUDA driver version as `major*1000 + minor*10` (12020 == CUDA 12.2).
    cuda_driver_version: i32,
}

impl Nvml {
    fn new() -> Self {
        Nvml {
            inited: false,
            desc: CudaDeviceDesc::apple_default(8u64 << 30),
            serial: "HL-SIM-00000001".to_string(),
            driver_version: "535.230.02".to_string(),
            nvml_version: "12.535.230.02".to_string(),
            cuda_driver_version: 12020,
        }
    }

    /// Seed device identity from the launcher's `HL_CUDA_*` env (idempotent on repeat init).
    fn seed_from_env(&mut self) {
        if let Ok(name) = std::env::var("HL_CUDA_NAME") {
            if !name.is_empty() {
                self.desc.name = name;
            }
        }
        // Compute capability "maj.min" (HL_CUDA_CC), e.g. "8.6".
        if let Ok(cc) = std::env::var("HL_CUDA_CC") {
            let mut it = cc.split('.');
            if let Some(maj) = it.next().and_then(|s| s.trim().parse::<u32>().ok()) {
                let min = it
                    .next()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(0);
                self.desc.compute_capability = (maj, min);
            }
        }
        // Reported VRAM: prefer the launcher's byte-exact HL_CUDA_VRAM_BYTES, else the C-oracle's
        // HL_CUDA_VRAM (megabytes).
        if let Some(bytes) = std::env::var("HL_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            self.desc.total_mem = bytes;
        } else if let Some(mb) = std::env::var("HL_CUDA_VRAM")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            if mb > 0 {
                self.desc.total_mem = mb * 1024 * 1024;
            }
        }
        // Driver / NVML version handshake (nvidia-smi aborts on a driver-major mismatch).
        if let Ok(drv) = std::env::var("HL_CUDA_DRIVER") {
            if !drv.is_empty() {
                self.nvml_version = format!("12.{drv}");
                self.driver_version = drv;
            }
        }
        if let Ok(nv) = std::env::var("HL_CUDA_NVML") {
            if !nv.is_empty() {
                self.nvml_version = nv;
            }
        }
        if let Ok(cd) = std::env::var("HL_CUDA_DRIVER_CUDA") {
            let mut it = cd.split('.');
            if let Some(maj) = it.next().and_then(|s| s.trim().parse::<i32>().ok()) {
                let min = it
                    .next()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .unwrap_or(0);
                self.cuda_driver_version = maj * 1000 + min * 10;
            }
        }
    }
}

impl Nvml {
    fn with<R>(f: impl FnOnce(&mut Nvml) -> R) -> R {
        static STATE: OnceLock<Mutex<Nvml>> = OnceLock::new();
        let state = STATE.get_or_init(|| Mutex::new(Nvml::new()));
        let mut nvml = state.lock().unwrap_or_else(|error| error.into_inner());
        f(&mut nvml)
    }

    /// Is `device` the one valid device handle?
    fn is_valid(device: *mut c_void) -> bool {
        device as usize == DEVICE_TOKEN
    }
}

/// Copy `s` (with a trailing nul) into the caller's `dst[..len]` buffer, truncating to fit. A null
/// buffer or zero length is `NVML_ERROR_INVALID_ARGUMENT`.
unsafe fn write_cstr(dst: *mut c_char, len: u32, s: &str) -> i32 {
    if dst.is_null() || len == 0 {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let cap = (len as usize).saturating_sub(1);
    let n = s.len().min(cap);
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, n);
    *dst.add(n) = 0;
    NVML_SUCCESS
}

// ==================================================================================================
// init / shutdown / error strings
// ==================================================================================================

fn init_impl() -> i32 {
    Nvml::with(|s| {
        if !s.inited {
            s.seed_from_env();
            s.inited = true;
        }
    });
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlInit_v2() -> i32 {
    init_impl()
}

#[no_mangle]
pub extern "C" fn nvmlInit() -> i32 {
    init_impl()
}

#[no_mangle]
pub extern "C" fn nvmlInitWithFlags(_flags: u32) -> i32 {
    init_impl()
}

#[no_mangle]
pub extern "C" fn nvmlShutdown() -> i32 {
    Nvml::with(|s| s.inited = false);
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlErrorString(result: i32) -> *const c_char {
    let s: &'static [u8] = match result {
        0 => b"The operation was successful\0",
        1 => b"NVML was not first initialized with nvmlInit()\0",
        2 => b"A supplied argument is invalid\0",
        3 => b"The requested operation is not available on target device\0",
        4 => b"The current user does not have permission for operation\0",
        5 => b"NVML was already initialized\0",
        6 => b"A query to find an object was unsuccessful\0",
        7 => b"An input argument is not large enough\0",
        9 => b"The NVIDIA driver is not loaded\0",
        _ => b"An internal driver error occurred\0",
    };
    s.as_ptr() as *const c_char
}

// ==================================================================================================
// system-level version queries
// ==================================================================================================

#[no_mangle]
pub extern "C" fn nvmlSystemGetDriverVersion(v: *mut c_char, len: u32) -> i32 {
    Nvml::with(|s| unsafe { write_cstr(v, len, &s.driver_version) })
}

#[no_mangle]
pub extern "C" fn nvmlSystemGetNVMLVersion(v: *mut c_char, len: u32) -> i32 {
    Nvml::with(|s| unsafe { write_cstr(v, len, &s.nvml_version) })
}

impl Nvml {
    fn cuda_driver_version(v: *mut i32) -> i32 {
        if v.is_null() {
            return NVML_ERROR_INVALID_ARGUMENT;
        }
        Self::with(|s| unsafe { *v = s.cuda_driver_version });
        NVML_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn nvmlSystemGetCudaDriverVersion(v: *mut i32) -> i32 {
    Nvml::cuda_driver_version(v)
}

#[no_mangle]
pub extern "C" fn nvmlSystemGetCudaDriverVersion_v2(v: *mut i32) -> i32 {
    Nvml::cuda_driver_version(v)
}

#[no_mangle]
pub extern "C" fn nvmlSystemGetProcessName(_pid: u32, name: *mut c_char, len: u32) -> i32 {
    // The simulated device runs no named compute processes; report an empty name.
    unsafe { write_cstr(name, len, "") }
}

// ==================================================================================================
// device enumeration
// ==================================================================================================

impl Nvml {
    fn count(count: *mut u32) -> i32 {
        if Self::with(|s| !s.inited) {
            return NVML_ERROR_UNINITIALIZED;
        }
        if count.is_null() {
            return NVML_ERROR_INVALID_ARGUMENT;
        }
        unsafe { *count = 1 };
        NVML_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCount(count: *mut u32) -> i32 {
    Nvml::count(count)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCount_v2(count: *mut u32) -> i32 {
    Nvml::count(count)
}

impl Nvml {
    fn handle_by_index(index: u32, dev: *mut *mut c_void) -> i32 {
        if Self::with(|s| !s.inited) {
            return NVML_ERROR_UNINITIALIZED;
        }
        if dev.is_null() || index != 0 {
            return NVML_ERROR_INVALID_ARGUMENT;
        }
        unsafe { *dev = DEVICE_TOKEN as *mut c_void };
        NVML_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByIndex(index: u32, dev: *mut *mut c_void) -> i32 {
    Nvml::handle_by_index(index, dev)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByIndex_v2(index: u32, dev: *mut *mut c_void) -> i32 {
    Nvml::handle_by_index(index, dev)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByUUID(uuid: *const c_char, dev: *mut *mut c_void) -> i32 {
    if Nvml::with(|s| !s.inited) {
        return NVML_ERROR_UNINITIALIZED;
    }
    if uuid.is_null() || dev.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let requested = unsafe { std::ffi::CStr::from_ptr(uuid) }.to_string_lossy();
    let ours = Nvml::with(|s| s.desc.uuid_str());
    if requested != ours {
        return NVML_ERROR_NOT_FOUND;
    }
    unsafe { *dev = DEVICE_TOKEN as *mut c_void };
    NVML_SUCCESS
}

impl Nvml {
    fn handle_by_pci(pci: *const c_char, dev: *mut *mut c_void) -> i32 {
        if Self::with(|s| !s.inited) {
            return NVML_ERROR_UNINITIALIZED;
        }
        if pci.is_null() || dev.is_null() {
            return NVML_ERROR_INVALID_ARGUMENT;
        }
        // Only one device on a fixed bus; accept any well-formed id.
        unsafe { *dev = DEVICE_TOKEN as *mut c_void };
        NVML_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByPciBusId(pci: *const c_char, dev: *mut *mut c_void) -> i32 {
    Nvml::handle_by_pci(pci, dev)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByPciBusId_v2(
    pci: *const c_char,
    dev: *mut *mut c_void,
) -> i32 {
    Nvml::handle_by_pci(pci, dev)
}

// ==================================================================================================
// per-device identity
// ==================================================================================================

#[no_mangle]
pub extern "C" fn nvmlDeviceGetName(dev: *mut c_void, name: *mut c_char, len: u32) -> i32 {
    if !Nvml::is_valid(dev) {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    Nvml::with(|s| unsafe { write_cstr(name, len, &s.desc.name) })
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetUUID(dev: *mut c_void, uuid: *mut c_char, len: u32) -> i32 {
    if !Nvml::is_valid(dev) {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    Nvml::with(|s| unsafe { write_cstr(uuid, len, &s.desc.uuid_str()) })
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetSerial(dev: *mut c_void, serial: *mut c_char, len: u32) -> i32 {
    if !Nvml::is_valid(dev) {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    Nvml::with(|s| unsafe { write_cstr(serial, len, &s.serial) })
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetIndex(dev: *mut c_void, index: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || index.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *index = 0 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMinorNumber(dev: *mut c_void, minor: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || minor.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *minor = 0 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetBrand(dev: *mut c_void, brand: *mut i32) -> i32 {
    if !Nvml::is_valid(dev) || brand.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *brand = NVML_BRAND_NVS };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCudaComputeCapability(
    dev: *mut c_void,
    major: *mut i32,
    minor: *mut i32,
) -> i32 {
    if !Nvml::is_valid(dev) || major.is_null() || minor.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    Nvml::with(|s| unsafe {
        *major = s.desc.compute_capability.0 as i32;
        *minor = s.desc.compute_capability.1 as i32;
    });
    NVML_SUCCESS
}

/// `nvmlDeviceGetArchitecture` — the NVML architecture enum inferred from the compute-capability major
/// (KEPLER=2 MAXWELL=3 PASCAL=4 VOLTA=5 TURING=6 AMPERE=7 ADA=8 HOPPER=9).
#[no_mangle]
pub extern "C" fn nvmlDeviceGetArchitecture(dev: *mut c_void, arch: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || arch.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    Nvml::with(|s| {
        let (maj, min) = s.desc.compute_capability;
        let a: u32 = match maj {
            3 => 2,
            5 => 3,
            6 => 4,
            7 => {
                if min >= 5 {
                    6
                } else {
                    5
                }
            }
            8 => {
                if min >= 9 {
                    8
                } else {
                    7
                }
            }
            9 => 9,
            _ => 7,
        };
        unsafe { *arch = a };
    });
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetVbiosVersion(dev: *mut c_void, v: *mut c_char, len: u32) -> i32 {
    if !Nvml::is_valid(dev) {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { write_cstr(v, len, "00.00.00.00.00") }
}

// ==================================================================================================
// memory / utilization
// ==================================================================================================

/// `nvmlMemory_t` (v1) — `{ total, free, used }` in bytes.
#[repr(C)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

/// `nvmlMemory_v2_t` — version-tagged with an extra `reserved` field. The caller sets `version`; we keep
/// it and fill the byte counts.
#[repr(C)]
struct NvmlMemoryV2 {
    version: u32,
    total: u64,
    reserved: u64,
    free: u64,
    used: u64,
}

#[repr(C)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMemoryInfo(dev: *mut c_void, m: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || m.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let total = Nvml::with(|s| s.desc.total_mem);
    let out = unsafe { &mut *(m as *mut NvmlMemory) };
    out.total = total;
    out.used = 0;
    out.free = total;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMemoryInfo_v2(dev: *mut c_void, m: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || m.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let total = Nvml::with(|s| s.desc.total_mem);
    let out = unsafe { &mut *(m as *mut NvmlMemoryV2) };
    // Keep the caller-provided version tag; fill the v2 byte counts.
    out.total = total;
    out.reserved = 0;
    out.used = 0;
    out.free = total;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetUtilizationRates(dev: *mut c_void, u: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || u.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    // Idle simulated device: no outstanding compute → 0% GPU / 0% memory-controller utilization.
    let out = unsafe { &mut *(u as *mut NvmlUtilization) };
    out.gpu = 0;
    out.memory = 0;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetEncoderUtilization(
    dev: *mut c_void,
    util: *mut u32,
    sampling: *mut u32,
) -> i32 {
    if !Nvml::is_valid(dev) || util.is_null() || sampling.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe {
        *util = 0;
        *sampling = 167000; // sampling period (µs), like the C oracle
    }
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetDecoderUtilization(
    dev: *mut c_void,
    util: *mut u32,
    sampling: *mut u32,
) -> i32 {
    if !Nvml::is_valid(dev) || util.is_null() || sampling.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe {
        *util = 0;
        *sampling = 167000;
    }
    NVML_SUCCESS
}

// ==================================================================================================
// PCI info
// ==================================================================================================

/// `nvmlPciInfo_t` (current / _v3 layout) — the exact `#[repr(C)]` field order/size is the ABI contract.
#[repr(C)]
struct NvmlPciInfo {
    bus_id_legacy: [c_char; 16],
    domain: u32,
    bus: u32,
    device: u32,
    pci_device_id: u32,
    pci_sub_system_id: u32,
    bus_id: [c_char; 32],
}

/// Write `s` (nul-terminated, truncated to fit) into a fixed C char array field.
impl NvmlPciInfo {
    unsafe fn fill(field: &mut [c_char], value: &str) {
        let cap = field.len().saturating_sub(1);
        let length = value.len().min(cap);
        std::ptr::copy_nonoverlapping(value.as_ptr(), field.as_mut_ptr() as *mut u8, length);
        field[length] = 0;
    }

    fn write(device: *mut c_void, output: *mut c_void) -> i32 {
        if !Nvml::is_valid(device) || output.is_null() {
            return NVML_ERROR_INVALID_ARGUMENT;
        }
        let out = unsafe { &mut *(output as *mut Self) };
        unsafe {
            core::ptr::write_bytes(out as *mut Self as *mut u8, 0, core::mem::size_of::<Self>())
        };
        out.domain = 0;
        out.bus = 0;
        out.device = 0;
        out.pci_device_id = 0x1EB8_10DE; // fabricated device:vendor (vendor 0x10DE = NVIDIA)
        out.pci_sub_system_id = 0x1EB8_10DE;
        unsafe {
            Self::fill(&mut out.bus_id, "00000000:00:00.0");
            Self::fill(&mut out.bus_id_legacy, "0000:00:00.0");
        }
        NVML_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPciInfo_v3(dev: *mut c_void, p: *mut c_void) -> i32 {
    NvmlPciInfo::write(dev, p)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPciInfo_v2(dev: *mut c_void, p: *mut c_void) -> i32 {
    NvmlPciInfo::write(dev, p)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPciInfo(dev: *mut c_void, p: *mut c_void) -> i32 {
    NvmlPciInfo::write(dev, p)
}

/// `nvmlDeviceGetPciInfoExt` — the extended PCI-info struct is an undocumented private layout; honestly
/// report it unsupported (nvidia-smi falls back to the public `nvmlDeviceGetPciInfo`).
#[no_mangle]
pub extern "C" fn nvmlDeviceGetPciInfoExt(dev: *mut c_void, _p: *mut c_void) -> i32 {
    if Nvml::is_valid(dev) {
        NVML_ERROR_NOT_SUPPORTED
    } else {
        NVML_ERROR_INVALID_ARGUMENT
    }
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCurrPcieLinkGeneration(dev: *mut c_void, g: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || g.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *g = 4 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMaxPcieLinkGeneration(dev: *mut c_void, g: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || g.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *g = 4 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCurrPcieLinkWidth(dev: *mut c_void, w: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || w.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *w = 16 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMaxPcieLinkWidth(dev: *mut c_void, w: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || w.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *w = 16 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMemoryBusWidth(dev: *mut c_void, w: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || w.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *w = 256 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetNumGpuCores(dev: *mut c_void, n: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || n.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *n = 4096 };
    NVML_SUCCESS
}

// ==================================================================================================
// sensors / clocks / power (modeled sane values for a mid-range card)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn nvmlDeviceGetTemperature(dev: *mut c_void, _sensor: i32, t: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || t.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *t = 35 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetTemperatureThreshold(
    dev: *mut c_void,
    _kind: u32,
    t: *mut u32,
) -> i32 {
    if !Nvml::is_valid(dev) || t.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *t = 90 }; // slowdown/shutdown threshold
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPowerUsage(dev: *mut c_void, mw: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || mw.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *mw = 25000 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPowerManagementLimit(dev: *mut c_void, mw: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || mw.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *mw = 70000 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetEnforcedPowerLimit(dev: *mut c_void, mw: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || mw.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *mw = 70000 };
    NVML_SUCCESS
}

fn clock_impl(dev: *mut c_void, clock_type: i32, mhz: *mut u32) -> i32 {
    if !Nvml::is_valid(dev) || mhz.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe {
        *mhz = if clock_type == NVML_CLOCK_MEM {
            6000
        } else {
            1500
        }
    };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetClockInfo(dev: *mut c_void, clock_type: i32, mhz: *mut u32) -> i32 {
    clock_impl(dev, clock_type, mhz)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMaxClockInfo(
    dev: *mut c_void,
    clock_type: i32,
    mhz: *mut u32,
) -> i32 {
    clock_impl(dev, clock_type, mhz)
}

/// `nvmlDeviceGetFanSpeed` — an Apple GPU exposes no per-GPU fan tachometer; honestly unsupported
/// (nvidia-smi → "N/A").
#[no_mangle]
pub extern "C" fn nvmlDeviceGetFanSpeed(dev: *mut c_void, _pct: *mut u32) -> i32 {
    if Nvml::is_valid(dev) {
        NVML_ERROR_NOT_SUPPORTED
    } else {
        NVML_ERROR_INVALID_ARGUMENT
    }
}

// ==================================================================================================
// modes / states
// ==================================================================================================

fn enable_state(dev: *mut c_void, m: *mut i32, value: i32) -> i32 {
    if !Nvml::is_valid(dev) || m.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *m = value };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPersistenceMode(dev: *mut c_void, m: *mut i32) -> i32 {
    enable_state(dev, m, NVML_FEATURE_DISABLED)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetDisplayMode(dev: *mut c_void, m: *mut i32) -> i32 {
    enable_state(dev, m, NVML_FEATURE_DISABLED)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetComputeMode(dev: *mut c_void, m: *mut i32) -> i32 {
    if !Nvml::is_valid(dev) || m.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *m = NVML_COMPUTEMODE_DEFAULT };
    NVML_SUCCESS
}

impl Nvml {
    fn performance_state(device: *mut c_void, state: *mut i32) -> i32 {
        if !Self::is_valid(device) || state.is_null() {
            return NVML_ERROR_INVALID_ARGUMENT;
        }
        unsafe { *state = NVML_PSTATE_0 }; // maximum-performance state (idle unified device)
        NVML_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPerformanceState(dev: *mut c_void, p: *mut i32) -> i32 {
    Nvml::performance_state(dev, p)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetPowerState(dev: *mut c_void, p: *mut i32) -> i32 {
    Nvml::performance_state(dev, p)
}

/// `nvmlDeviceGetMigMode` — MIG partitioning is unsupported on the simulated device (nvidia-smi → "N/A").
#[no_mangle]
pub extern "C" fn nvmlDeviceGetMigMode(dev: *mut c_void, _cur: *mut u32, _pend: *mut u32) -> i32 {
    if Nvml::is_valid(dev) {
        NVML_ERROR_NOT_SUPPORTED
    } else {
        NVML_ERROR_INVALID_ARGUMENT
    }
}

// ==================================================================================================
// running processes (none)
// ==================================================================================================

/// Shared body for the compute/graphics running-process queries: the simulated device runs no external
/// processes, so report a count of zero and leave the caller's array untouched.
fn no_procs(dev: *mut c_void, count: *mut u32, _infos: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || count.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *count = 0 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetComputeRunningProcesses_v3(
    dev: *mut c_void,
    c: *mut u32,
    i: *mut c_void,
) -> i32 {
    no_procs(dev, c, i)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetComputeRunningProcesses_v2(
    dev: *mut c_void,
    c: *mut u32,
    i: *mut c_void,
) -> i32 {
    no_procs(dev, c, i)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetComputeRunningProcesses(
    dev: *mut c_void,
    c: *mut u32,
    i: *mut c_void,
) -> i32 {
    no_procs(dev, c, i)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetGraphicsRunningProcesses_v3(
    dev: *mut c_void,
    c: *mut u32,
    i: *mut c_void,
) -> i32 {
    no_procs(dev, c, i)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetGraphicsRunningProcesses_v2(
    dev: *mut c_void,
    c: *mut u32,
    i: *mut c_void,
) -> i32 {
    no_procs(dev, c, i)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetGraphicsRunningProcesses(
    dev: *mut c_void,
    c: *mut u32,
    i: *mut c_void,
) -> i32 {
    no_procs(dev, c, i)
}

// ==================================================================================================
// private/internal export table (the "dark API")
// ==================================================================================================

/// The version-handshake slot every populated internal-table entry points at: report the query
/// unsupported so nvidia-smi's list/query modes fall back to the PUBLIC NVML API this shim implements.
extern "C" fn hl_et_notsup() -> i32 {
    NVML_ERROR_NOT_SUPPORTED
}

const HL_ET_SLOTS: usize = 245; // matches real libnvidia-ml.so.535.230.02
const HL_ET_HEADER: usize = 0x7a8; // real slot[0] value (table byte size)

/// Build the internal export table once and leak it (nvidia-smi keeps the pointer for the process
/// lifetime). Returns its stable address as a `usize` (the slots are `void*`-sized).
struct ExportTable;

impl ExportTable {
    fn address() -> usize {
        static ADDR: OnceLock<usize> = OnceLock::new();
        *ADDR.get_or_init(|| {
            // NULL slot positions observed in the real table (besides the header at [0]).
            const NULLS: &[usize] = &[
                1, 2, 24, 35, 60, 64, 90, 104, 121, 122, 139, 150, 157, 158, 159, 160, 161, 162,
                163, 167, 176, 177, 178, 187, 190, 191, 198, 201, 202, 207, 211, 216, 217, 235,
                236,
            ];
            let notsup = hl_et_notsup as *const () as usize;
            let mut table: Vec<usize> = vec![notsup; HL_ET_SLOTS];
            table[0] = HL_ET_HEADER;
            for &k in NULLS {
                table[k] = 0;
            }
            // Leak the table so the returned pointer stays valid for the process lifetime.
            Box::leak(table.into_boxed_slice()).as_ptr() as usize
        })
    }
}

/// `nvmlInternalGetExportTable(ppExportTable, pExportTableId)` — the undocumented private symbol
/// nvidia-smi resolves right after init for a version handshake. A valid non-null table clears the
/// "Mismatch in versions" abort; every populated slot returns `NVML_ERROR_NOT_SUPPORTED`, which steers
/// the list/query render paths onto our public API. See `hl-gpu/nvml/nvml_shim.c` for the full RE notes.
#[no_mangle]
pub extern "C" fn nvmlInternalGetExportTable(
    pp_export_table: *mut *const c_void,
    _p_export_table_id: *mut c_void,
) -> i32 {
    if pp_export_table.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *pp_export_table = ExportTable::address() as *const c_void };
    NVML_SUCCESS
}
