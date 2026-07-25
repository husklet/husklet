use super::*;

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
