//! elf210 — #210 x86_64 ELF-loader fixed-base collision fallback. Owner: elf210 agent. Edit ONLY this file.
//!
//! Under HL_JIT_PCACHE the x86_64 loader (translate/x86_64/elf.c load_elf) maps the guest image at a FIXED
//! VA (PC_IMG_BASE) so the translated arena is byte-identical across runs and thus cache-revivable. If that
//! VA is already occupied (a prior mapping, ASLR, 16KiB-host vs 4KiB-guest page rounding), the MAP_FIXED
//! returns MAP_FAILED. Pre-fix load_elf `exit(1)`'d there (#210 — a load-time crash, same class as #207).
//! It now falls back to a kernel-chosen base and latches g_force_base_failed so the pcache neither restores
//! a fixed-base file over the mixed-base arena nor persists one — byte-exact execution, just not cached this
//! run. This mirrors the aarch64 loader (os/linux/elf.c) + its g_force_base_failed gate.
//!
//! HL_X_FORCE_BASE_COLLIDE=1 (a load_elf test hook, gated + inert otherwise) deterministically forces that
//! collision on every fixed-VA load — the geometry never triggers it naturally (the two fixed bases sit
//! 512GB apart). We run the guest under HL_JIT_PCACHE=1 + the collision hook and assert byte-exactness vs the
//! qemu-x86_64 oracle: proof the fallback path loads + runs the image correctly. x86_64-only (the arm64 lane
//! already had this fallback). Keep this module compiling at all times.
#![allow(unused_imports)]
use crate::support::{group, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![elf210()]
}

fn elf210() -> Group {
    group(
        "elf210",
        vec![src("elf210-basecollide", "elf210.c")
            .only(&[Engine::LinuxX86_64])
            .env("HL_JIT_PCACHE", "1")
            .env("HL_JIT_PCACHE_DIR", "/tmp/hljit-pcache-elf210")
            .env("HL_X_FORCE_BASE_COLLIDE", "1")
            .oracle()],
    )
}
