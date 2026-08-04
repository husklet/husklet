pub(crate) mod atomic;
pub(crate) mod decode;
pub(crate) mod exit;
pub(crate) mod fp;
pub(crate) mod memory;
pub(crate) mod simd;
pub(crate) mod system;

pub(crate) mod coordinate;
pub(crate) mod crc32;
pub(crate) mod integer;
mod integer_decode;
pub(crate) mod integer_support;
pub(crate) mod interpreter;
pub(crate) mod ir;
pub(crate) mod register;
pub(crate) mod runtime;
pub(crate) mod shift;
pub(crate) mod softfloat;
pub(crate) mod state;

pub use exit::Aarch64ExecutionExit;

#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use self::{fp as aarch64_fp, integer_support as aarch64_integer_support, memory as aarch64_memory};

pub(crate) use simd::{
    aes as aarch64_simd_aes, arithmetic as aarch64_simd_arithmetic, bf16 as aarch64_simd_bf16,
    compare as aarch64_simd_compare, convert as aarch64_simd_convert, crypto as aarch64_simd_crypto,
    decode as aarch64_simd_decode, dot as aarch64_simd_dot, fcvtzs as aarch64_simd_fcvtzs,
    fp_product as aarch64_simd_fp_product, fp_reduce as aarch64_simd_fp_reduce, fused as aarch64_simd_fused,
    high_product as aarch64_simd_high_product, immediate as aarch64_simd_immediate,
    interpreter as aarch64_simd_interpreter, lane as aarch64_simd_lane_interpreter,
    long_product as aarch64_simd_long_product, matrix as aarch64_simd_matrix, narrow as aarch64_simd_narrow,
    pair as aarch64_simd_pair, product as aarch64_simd_product, reciprocal as aarch64_simd_reciprocal,
    reduce as aarch64_simd_reduce_interpreter, saturating_product as aarch64_simd_saturating_product,
    saturating_unary as aarch64_simd_saturating_unary, scalar as aarch64_simd_scalar, scvtf as aarch64_simd_scvtf,
    sha1 as aarch64_simd_sha1, sha256 as aarch64_simd_sha256, variable as aarch64_simd_variable,
    wide as aarch64_simd_wide_interpreter,
};

#[cfg(test)]
mod test;
