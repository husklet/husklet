//! CUDA **texture objects** over 2D arrays — the driver-side model of `cudaMallocArray` /
//! `cudaMemcpy2DToArray` / `cudaCreateTextureObject` / `tex2D`.
//!
//! ## Where the boundary is (read this before assuming a kernel `tex2D` works)
//! A real `tex2D(texObj, x, y)` is a **kernel** instruction (`tex.2d.v4.f32.f32 …` in PTX) served by
//! the GPU's texture unit. This crate lowers kernels to the neutral kernel-IR
//! ([`hl_gpu::protocol::model::kernel`]) whose interpreter lives in the `hl-gpu` executor — and that
//! interpreter models **no** `tex` instruction (its opcode set is the SIMT ALU/mem/atomic subset). So the
//! texture *unit* itself is modeled **here, in the driver**, host-side: a [`CudaArray`] holds the texel
//! data, a [`TextureObject`] binds it to a [`SamplerDesc`], and [`TextureObject::tex2d`] performs the
//! fetch/filter exactly. This is a real, deterministic computation of the CUDA texture-fetch semantics —
//! not a stub returning a placeholder. What is honestly *not* modeled is a `tex.2d` PTX opcode flowing
//! through the kernel interpreter; that would require an `hl-gpu` change (out of this crate's scope).
//!
//! The filter math matches CUDA's documented unnormalized-coordinate behaviour (CUDA C Programming Guide,
//! "Texture Fetching"): point mode returns `T[floor(y)][floor(x)]`; linear mode bilinearly interpolates
//! the four texels around `(x-0.5, y-0.5)` with the fractional weights held in 1.8 fixed point (8 fraction
//! bits), which is a no-op at the exactly-representable sample positions the demos assert.

/// Texture filter mode (`cudaTextureFilterMode`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterMode {
    /// `cudaFilterModePoint` — nearest texel, no interpolation.
    Point,
    /// `cudaFilterModeLinear` — bilinear interpolation of the 4 surrounding texels.
    Linear,
}

/// Texture addressing mode (`cudaTextureAddressMode`) for out-of-range coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddressMode {
    /// `cudaAddressModeClamp` — clamp the index to `[0, extent-1]` (the default for unnormalized coords).
    Clamp,
    /// `cudaAddressModeWrap` — wrap the index modulo the extent.
    Wrap,
}

/// A `cudaTextureDesc` (the subset the model honours): filter + per-axis addressing + coordinate mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SamplerDesc {
    pub filter: FilterMode,
    pub address_x: AddressMode,
    pub address_y: AddressMode,
    /// `false` → unnormalized coordinates in texels (what the demos use); `true` → coordinates in `[0,1)`.
    pub normalized: bool,
}

impl SamplerDesc {
    /// The common demo default: linear filter, clamp addressing, unnormalized texel coordinates.
    pub fn linear_clamp() -> Self {
        Self {
            filter: FilterMode::Linear,
            address_x: AddressMode::Clamp,
            address_y: AddressMode::Clamp,
            normalized: false,
        }
    }

    /// Point (nearest) filter, clamp addressing, unnormalized coordinates.
    pub fn point_clamp() -> Self {
        Self {
            filter: FilterMode::Point,
            ..Self::linear_clamp()
        }
    }
}

/// A `cudaArray` of single-channel `f32` texels laid out row-major (`width` × `height`). Opaque device
/// memory in CUDA; modeled host-side here since the texture unit is evaluated in the driver.
#[derive(Clone, PartialEq, Debug)]
pub struct CudaArray {
    /// Opaque handle (a real, monotonic id so two arrays are never confused).
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// Row-major texel storage (`width * height` elements). Populated by `cudaMemcpy2DToArray`.
    pub texels: Vec<f32>,
}

impl CudaArray {
    /// Read texel `(col, row)` with the two axes clamped/wrapped per `desc`. `col`/`row` may be negative
    /// (linear filtering reaches one texel to the left/up) — addressing is applied before the fetch.
    fn texel(&self, col: i64, row: i64, desc: &SamplerDesc) -> f32 {
        let c = address(col, self.width as i64, desc.address_x);
        let r = address(row, self.height as i64, desc.address_y);
        self.texels[(r as usize) * (self.width as usize) + (c as usize)]
    }
}

/// Apply an [`AddressMode`] to map a (possibly out-of-range, possibly negative) index into `[0, extent-1]`.
fn address(idx: i64, extent: i64, mode: AddressMode) -> i64 {
    match mode {
        AddressMode::Clamp => idx.clamp(0, extent - 1),
        AddressMode::Wrap => idx.rem_euclid(extent),
    }
}

/// Quantize a filter fractional weight to CUDA's 1.8 fixed point (8 fraction bits), round-to-nearest.
/// At exactly-representable fractions (`0.0`, `0.5`, …) this is the identity, so a midpoint sample is exact.
struct FilterWeight(f32);

impl FilterWeight {
    fn quantized(&self) -> f32 {
        (self.0 * 256.0).round() / 256.0
    }
}

/// A `cudaTextureObject_t`: a [`CudaArray`] bound to a [`SamplerDesc`]. Owns a copy of the texel data so a
/// fetch is a pure function of the object (mirroring how a bound texture is immutable for its lifetime).
#[derive(Clone, PartialEq, Debug)]
pub struct TextureObject {
    pub id: u64,
    pub array: CudaArray,
    pub desc: SamplerDesc,
}

impl TextureObject {
    /// `tex2D<float>(texObj, x, y)` — fetch/filter exactly per the object's [`SamplerDesc`].
    ///
    /// * **Point**: returns `T[floor(y)][floor(x)]` (addressing applied to the two indices).
    /// * **Linear**: bilinearly interpolates the four texels around `(x-0.5, y-0.5)` with 1.8 fixed-point
    ///   fractional weights — i.e. `(1-a)(1-b)·t00 + a(1-b)·t10 + (1-a)b·t01 + a·b·t11`.
    ///
    /// Coordinates are unnormalized texels when `desc.normalized == false`; otherwise they are first scaled
    /// by the array extent (`x * width`, `y * height`).
    pub fn tex2d(&self, x: f32, y: f32) -> f32 {
        let (mut xs, mut ys) = (x, y);
        if self.desc.normalized {
            xs = x * self.array.width as f32;
            ys = y * self.array.height as f32;
        }
        match self.desc.filter {
            FilterMode::Point => {
                let col = xs.floor() as i64;
                let row = ys.floor() as i64;
                self.array.texel(col, row, &self.desc)
            }
            FilterMode::Linear => {
                let xb = xs - 0.5;
                let yb = ys - 0.5;
                let i0 = xb.floor();
                let j0 = yb.floor();
                let a = FilterWeight(xb - i0).quantized();
                let b = FilterWeight(yb - j0).quantized();
                let (ci, cj) = (i0 as i64, j0 as i64);
                let t00 = self.array.texel(ci, cj, &self.desc);
                let t10 = self.array.texel(ci + 1, cj, &self.desc);
                let t01 = self.array.texel(ci, cj + 1, &self.desc);
                let t11 = self.array.texel(ci + 1, cj + 1, &self.desc);
                // Same evaluation order a CPU bilinear reference uses, so exactly-weighted samples are
                // bit-exact.
                let top = (1.0 - a) * t00 + a * t10;
                let bot = (1.0 - a) * t01 + a * t11;
                (1.0 - b) * top + b * bot
            }
        }
    }
}
