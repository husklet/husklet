//! `cudaMemcpyToSymbol` / `cudaMemcpyFromSymbol` / `cudaGetSymbolAddress` — the `__device__` /
//! `__constant__` global-symbol copy path.
//!
//! nvcc lowers a `__constant__ float c[N];` / `__device__ int g;` to a `.const` / `.global` variable in
//! the module PTX. `cudaMemcpyToSymbol(c, src, n)` copies host bytes into that symbol's backing storage;
//! a kernel then reads it. These entry points resolve the symbol to its backing device buffer via
//! [`load_module::module_get_global`] (which lazily creates one buffer per symbol) and then reuse the
//! ordinary [`transfer`] copy path — so a symbol copy is a real `Cmd::WriteBuffer`/readback against the
//! symbol's device allocation, and a kernel handed that same device pointer reads exactly what the host
//! wrote. An unknown symbol is the honest `cudaErrorInvalidSymbol` analogue ([`GpuError::Invalid`]); an
//! over-long copy is bounded by the underlying [`transfer`] range check.

use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::service::{load_module, transfer};
use hl_gpu::{CommandSink, GpuError, Result};

/// `cudaGetSymbolAddress(&devPtr, symbol)` — the device pointer (and byte size) of module global `name`,
/// lazily creating its backing buffer on first lookup. Errors (`cudaErrorInvalidSymbol` analogue) if the
/// module declares no such `.global`/`.const` symbol.
pub fn get_symbol_address(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    module: u32,
    name: &str,
) -> Result<(DevicePtr, u64)> {
    load_module::module_get_global(ctx, sink, module, name)?
        .ok_or(GpuError::Invalid("cudaGetSymbolAddress: no such __device__/__constant__ symbol"))
}

/// `cudaMemcpyToSymbol(symbol, src, n, 0, cudaMemcpyHostToDevice)` — copy `src` into the symbol's backing
/// device buffer. The [`transfer::memcpy_htod`] range check bounds `n` against the symbol's declared size.
pub fn memcpy_to_symbol(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    module: u32,
    name: &str,
    src: &[u8],
) -> Result<()> {
    let (ptr, _size) = get_symbol_address(ctx, sink, module, name)?;
    transfer::memcpy_htod(ctx, sink, ptr, src)
}

/// `cudaMemcpyFromSymbol(dst, symbol, n, 0, cudaMemcpyDeviceToHost)` — read `n` bytes back from the
/// symbol's backing device buffer through the sink's device→host path.
pub fn memcpy_from_symbol(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    module: u32,
    name: &str,
    n: usize,
) -> Result<Vec<u8>> {
    let (ptr, _size) = get_symbol_address(ctx, sink, module, name)?;
    transfer::read_dtoh(ctx, sink, ptr, n)
}
