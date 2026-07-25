//! Raw byte/pointer/atomic primitives the interpreter step ([`super::exec`]) is built on.
//!
//! Ported verbatim (meaning-for-meaning) from `hl-gpu/src/ptx.rs`: `read_scalar` / `write_scalar` (little-
//! endian scalar load/store into a region), `shared_addr` (byte address from base+displacement),
//! `atomic_rmw` (a 32-bit read-modify-write, serialized because the CPU oracle is single-threaded within a
//! barrier phase), and the `ptr_int_add` / `ptr_target` pointer arithmetic over tagged `Val::P`.

use super::exec::Val;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::kernel::{
    ATOM_ADD, ATOM_AND, ATOM_CAS, ATOM_EXCH, ATOM_MAX, ATOM_MIN, ATOM_OR, ATOM_XOR,
};

/// Little-endian load of a `width`-byte scalar from `mem` at byte offset `at`, zero-extended to 64 bits.
pub(super) fn read_scalar(mem: &[u8], at: usize, width: usize) -> Result<u64> {
    if at + width > mem.len() {
        return Err(GpuError::OutOfBounds);
    }
    let mut b = [0u8; 8];
    b[..width].copy_from_slice(&mem[at..at + width]);
    Ok(u64::from_le_bytes(b))
}

/// Little-endian store of the low `width` bytes of `val` into `mem` at byte offset `at`.
pub(super) fn write_scalar(mem: &mut [u8], at: usize, width: usize, val: u64) -> Result<()> {
    if at + width > mem.len() {
        return Err(GpuError::OutOfBounds);
    }
    mem[at..at + width].copy_from_slice(&val.to_le_bytes()[..width]);
    Ok(())
}

/// Perform a 32-bit atomic read-modify-write on `mem` at byte offset `at`, returning the OLD value.
/// The CPU oracle is single-threaded within a barrier phase, so the read-modify-write is trivially
/// serialized (matching the total order a real atomic imposes).
pub(super) fn atomic_rmw(
    mem: &mut [u8],
    at: usize,
    op: u8,
    cmp: u32,
    val: u32,
    unsigned: bool,
) -> Result<u32> {
    let old = read_scalar(mem, at, 4)? as u32;
    let new = match op {
        ATOM_ADD => old.wrapping_add(val),
        ATOM_MIN => {
            if unsigned {
                old.min(val)
            } else {
                (old as i32).min(val as i32) as u32
            }
        }
        ATOM_MAX => {
            if unsigned {
                old.max(val)
            } else {
                (old as i32).max(val as i32) as u32
            }
        }
        ATOM_AND => old & val,
        ATOM_OR => old | val,
        ATOM_XOR => old ^ val,
        ATOM_EXCH => val,
        ATOM_CAS => {
            if old == cmp {
                val
            } else {
                old
            }
        }
        _ => return Err(GpuError::kernel("unknown atomic op")),
    };
    write_scalar(mem, at, 4, new as u64)?;
    Ok(old)
}

/// Integer add/sub with pointer awareness: `Ptr ± int → Ptr` (offset advanced), else a wrapping integer
/// (truncated to 32 bits unless `wide`). `sign` is `+1` for add, `-1` for sub.
pub(super) fn ptr_int_add(a: Val, b: Val, wide: bool, sign: i64) -> Val {
    match (a, b) {
        (Val::P { region, off }, _) => Val::P {
            region,
            off: off + sign * b.as_i64(),
        },
        (_, Val::P { region, off }) => Val::P {
            region,
            off: off + sign * a.as_i64(),
        },
        _ => {
            let r = a.as_i64().wrapping_add(sign.wrapping_mul(b.as_i64()));
            if wide {
                Val::I(r as u64)
            } else {
                Val::I(r as u32 as u64)
            }
        }
    }
}
