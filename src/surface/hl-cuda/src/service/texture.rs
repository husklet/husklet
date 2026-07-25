//! `cudaMallocArray` / `cudaMemcpy2DToArray` / `cudaCreateTextureObject` / `cudaDestroyTextureObject` /
//! `tex2D` — the texture-object service.
//!
//! These lower to the driver-side texture model ([`crate::model::texture`]) rather than to protocol
//! `Cmd`s: a `cudaArray` is opaque device memory and the texture *unit* (filtering) is evaluated in the
//! driver (see [`crate::model::texture`] for exactly why — the neutral kernel-IR interpreter models no
//! `tex` opcode). Every entry validates its inputs and returns a typed [`GpuError`] on misuse; none fakes
//! a success.

use crate::model::texture::{CudaArray, SamplerDesc, TextureObject};
use hl_gpu::{GpuError, Result};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic opaque-handle allocator shared by arrays and texture objects (non-zero, so `0` is invalid).
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn next_handle() -> u64 {
    NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)
}

/// `cudaMallocArray(&array, &channelDesc, width, height)` — allocate a `width × height` single-channel
/// `f32` array, zero-initialized. Errors on a zero extent (the `cudaErrorInvalidValue` analogue).
impl CudaArray {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(GpuError::Invalid("cudaMallocArray: zero width or height"));
        }
        let n = (width as usize)
            .checked_mul(height as usize)
            .ok_or(GpuError::Invalid("cudaMallocArray: width*height overflow"))?;
        Ok(CudaArray {
            id: next_handle(),
            width,
            height,
            texels: vec![0.0; n],
        })
    }

    /// `cudaMemcpy2DToArray(array, 0, 0, src, …, cudaMemcpyHostToDevice)` — upload a full row-major `f32`
    /// image into the array. Errors if `src.len()` is not exactly `width * height` texels.
    pub fn upload(&mut self, src: &[f32]) -> Result<()> {
        let expect = (self.width as usize) * (self.height as usize);
        if src.len() != expect {
            return Err(GpuError::Invalid(
                "cudaMemcpy2DToArray: source length != array texel count",
            ));
        }
        self.texels.copy_from_slice(src);
        Ok(())
    }
}

/// `cudaCreateTextureObject(&texObj, &resDesc{array}, &texDesc, null)` — bind `array` to `desc`, returning
/// a fetchable texture object. The object owns a snapshot of the array texels (a bound texture is immutable
/// for its lifetime).
impl TextureObject {
    pub fn from_array(array: &CudaArray, desc: SamplerDesc) -> Self {
        Self {
            id: next_handle(),
            array: array.clone(),
            desc,
        }
    }
}

/// `tex2D<float>(texObj, x, y)` — the exact fetch/filter (see [`TextureObject::tex2d`]).
pub fn tex2d(tex: &TextureObject, x: f32, y: f32) -> f32 {
    tex.tex2d(x, y)
}
