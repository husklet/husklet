use super::*;

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
