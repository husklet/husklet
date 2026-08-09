//! Runs a compiled guest of each ISA end to end through `Program::run`, the entry the shipped
//! `hl-aarch64` and `hl-x86_64` workers call, and pins the value the guest computed.

use super::*;
use crate::engine::ExitKind;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

const LINK_BASE: u64 = 0x40_0000;
const ENTRY_OFFSET: usize = 0x180;
const ROUNDS: u64 = 50_000;

/// `x86_64-linux-gnu-as` output for: acc=1; for i in 0..ROUNDS { acc = acc*33 + i; spill and reload
/// acc through the writable page at 0x401000 }; exit((acc >> 24) & 0xff).
const X86_CODE: &[u8] = &[
    0xb8, 0x01, 0x00, 0x00, 0x00, 0x31, 0xc9, 0x48, 0xbb, 0x00, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x48, 0x6b,
    0xc0, 0x21, 0x48, 0x01, 0xc8, 0x48, 0x89, 0x03, 0x48, 0x8b, 0x03, 0x48, 0xff, 0xc1, 0x48, 0x81, 0xf9, 0x50, 0xc3,
    0x00, 0x00, 0x72, 0xe7, 0x48, 0xc1, 0xe8, 0x18, 0x0f, 0xb6, 0xf8, 0xb8, 0x3c, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// `aarch64-linux-gnu-as` output for the same program.
const ARM_CODE: &[u8] = &[
    0x20, 0x00, 0x80, 0xd2, 0x01, 0x00, 0x80, 0xd2, 0x22, 0x04, 0x80, 0xd2, 0x03, 0x00, 0x82, 0xd2, 0x03, 0x08, 0xa0,
    0xf2, 0x04, 0x6a, 0x98, 0xd2, 0x00, 0x7c, 0x02, 0x9b, 0x00, 0x00, 0x01, 0x8b, 0x60, 0x00, 0x00, 0xf9, 0x60, 0x00,
    0x40, 0xf9, 0x21, 0x04, 0x00, 0x91, 0x3f, 0x00, 0x04, 0xeb, 0x43, 0xff, 0xff, 0x54, 0x00, 0xfc, 0x58, 0xd3, 0x00,
    0x1c, 0x40, 0x92, 0xa8, 0x0b, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
];

/// The value the guest must produce, derived here rather than pinned as a literal so the assertion
/// compares two independent evaluations of the same recurrence.
fn expected() -> i32 {
    let mut accumulator = 1_u64;
    for round in 0..ROUNDS {
        accumulator = accumulator.wrapping_mul(33).wrapping_add(round);
    }
    i32::try_from((accumulator >> 24) & 0xff).expect("byte fits")
}

fn image(code: &[u8], machine: u16, text_flags: u32) -> Vec<u8> {
    let mut bytes = vec![0_u8; 8192];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&(LINK_BASE + ENTRY_OFFSET as u64).to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
    // An executable text page and a separate writable page the loop spills its accumulator through.
    for (header, flags, offset) in [(64_usize, text_flags, 0_u64), (120, 6_u32, 4096)] {
        bytes[header..header + 4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[header + 4..header + 8].copy_from_slice(&flags.to_le_bytes());
        bytes[header + 8..header + 16].copy_from_slice(&offset.to_le_bytes());
        bytes[header + 16..header + 24].copy_from_slice(&(LINK_BASE + offset).to_le_bytes());
        bytes[header + 24..header + 32].copy_from_slice(&(LINK_BASE + offset).to_le_bytes());
        bytes[header + 32..header + 40].copy_from_slice(&4096_u64.to_le_bytes());
        bytes[header + 40..header + 48].copy_from_slice(&4096_u64.to_le_bytes());
        bytes[header + 48..header + 56].copy_from_slice(&4096_u64.to_le_bytes());
    }
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + code.len()].copy_from_slice(code);
    bytes
}

fn run(program: &str, code: &[u8], machine: u16, native: bool) -> Result<EngineExit, ProgramError> {
    run_with(program, code, machine, &[][..], native)
}

fn run_with(
    program: &str,
    code: &[u8],
    machine: u16,
    options: &[&str],
    native: bool,
) -> Result<EngineExit, ProgramError> {
    let identity = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("hl-gate-{program}-{}-{identity}", std::process::id()));
    std::fs::write(&path, image(code, machine, 5)).expect("write guest image");
    let mut arguments = vec![program.to_owned()];
    if native {
        arguments.push("--engine-option".to_owned());
        arguments.push("HL_NATIVE_EXECUTION=1".to_owned());
    }
    for option in options {
        arguments.push("--engine-option".to_owned());
        arguments.push((*option).to_owned());
    }
    arguments.push(path.to_string_lossy().into_owned());
    let result = Program::run(arguments);
    std::fs::remove_file(&path).expect("remove guest image");
    result
}

/// Every combination of guest ISA and execution mode must compute the same value. The x86 arm of
/// this engine is maintained by hand beside the aarch64 arm and has repeatedly been the one missing
/// a piece, so each arm is a case of its own rather than a variant of one.
#[test]
fn compiled_guests_agree_across_isas_and_modes() {
    let expected = expected();
    let mut failures = Vec::new();
    let mut ran = 0_usize;
    for (program, code, machine) in [("hl-x86_64", X86_CODE, 62_u16), ("hl-aarch64", ARM_CODE, 183_u16)] {
        for native in [false, true] {
            let label = format!("{program} native={native}");
            match run(program, code, machine, native) {
                Ok(exit) if exit.kind == ExitKind::Code && exit.guest_status == expected => ran += 1,
                Ok(exit) => failures.push(format!("{label}: expected code {expected}, got {exit:?}")),
                Err(error) => failures.push(format!("{label}: {error:?}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(ran, 4, "every ISA and mode must have run");
}

/// `aarch64-linux-gnu-as` output for: call `value_block` (`mov x0, #2; ret`) 64 times with a
/// `getpid` between calls so the block is translated and its admission is cached, `mprotect` the
/// text page writable, overwrite that `mov` with `mov x0, #17`, restore the page, then call the
/// block once more and exit with what it returned.
const ARM_SMC_CODE: &[u8] = &[
    0x13, 0x08, 0x80, 0xd2, 0x16, 0x00, 0x00, 0x94, 0x88, 0x15, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4, 0x73, 0x06, 0x00,
    0xf1, 0x81, 0xff, 0xff, 0x54, 0x00, 0x08, 0xa0, 0xd2, 0x01, 0x00, 0x82, 0xd2, 0xe2, 0x00, 0x80, 0xd2, 0x48, 0x1c,
    0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4, 0x81, 0x01, 0x00, 0x10, 0x02, 0x44, 0x80, 0x52, 0x02, 0x50, 0xba, 0x72, 0x22,
    0x00, 0x00, 0xb9, 0x00, 0x08, 0xa0, 0xd2, 0x01, 0x00, 0x82, 0xd2, 0xa2, 0x00, 0x80, 0xd2, 0x48, 0x1c, 0x80, 0xd2,
    0x01, 0x00, 0x00, 0xd4, 0x03, 0x00, 0x00, 0x94, 0xa8, 0x0b, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4, 0x40, 0x00, 0x80,
    0xd2, 0xc0, 0x03, 0x5f, 0xd6,
];

/// `x86_64-linux-gnu-as` output for the same program: `value_block` is `mov $2, %eax; ret` and the
/// rewrite stores the byte 17 over that immediate.
const X86_SMC_CODE: &[u8] = &[
    0xbb, 0x40, 0x00, 0x00, 0x00, 0xe8, 0x50, 0x00, 0x00, 0x00, 0xb8, 0x27, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xff, 0xcb,
    0x75, 0xf0, 0xbf, 0x00, 0x00, 0x40, 0x00, 0xbe, 0x00, 0x10, 0x00, 0x00, 0xba, 0x07, 0x00, 0x00, 0x00, 0xb8, 0x0a,
    0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x8d, 0x3d, 0x28, 0x00, 0x00, 0x00, 0xc6, 0x47, 0x01, 0x11, 0xbf, 0x00, 0x00,
    0x40, 0x00, 0xbe, 0x00, 0x10, 0x00, 0x00, 0xba, 0x05, 0x00, 0x00, 0x00, 0xb8, 0x0a, 0x00, 0x00, 0x00, 0x0f, 0x05,
    0xe8, 0x09, 0x00, 0x00, 0x00, 0x89, 0xc7, 0xb8, 0x3c, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xb8, 0x02, 0x00, 0x00, 0x00,
    0xc3,
];

/// A guest that rewrites a block it has already translated must observe the new code. The admission
/// cache serves a previous admission's code bytes, so this pins that a rewrite is still seen with
/// the cache on; returning 2 here is the shape of the stale-translation defect it must not
/// reintroduce. Both ISAs and both cache settings are separate cases because the x86 arm is
/// maintained by hand beside the aarch64 arm.
#[test]
fn a_rewritten_block_runs_its_new_code_with_and_without_the_admission_cache() {
    let mut failures = Vec::new();
    let mut ran = 0_usize;
    for (program, code, machine) in [
        ("hl-x86_64", X86_SMC_CODE, 62_u16),
        ("hl-aarch64", ARM_SMC_CODE, 183_u16),
    ] {
        for options in [&[][..], &["HL_NATIVE_ADMISSION_CACHE=1"][..]] {
            for native in [false, true] {
                let label = format!("{program} native={native} options={options:?}");
                match run_with(program, code, machine, options, native) {
                    Ok(exit) if exit.kind == ExitKind::Code && exit.guest_status == 17 => ran += 1,
                    Ok(exit) => failures.push(format!("{label}: expected code 17, got {exit:?}")),
                    Err(error) => failures.push(format!("{label}: {error:?}")),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(ran, 8, "every ISA, cache setting and mode must have run");
}
