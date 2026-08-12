#![cfg(target_os = "linux")]

mod engine;
mod guest;

use std::process::Command;

const ET_DYN: u16 = 3;
const PT_DYNAMIC: u32 = 2;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn assert_et_dyn(executable: &std::path::Path, require_rela: bool) {
    let bytes = std::fs::read(executable).unwrap();
    assert!(bytes.len() >= 64, "ELF header is truncated");
    assert_eq!(&bytes[..4], b"\x7fELF", "fixture is not ELF");
    assert_eq!(bytes[4], 2, "fixture ELF is not 64-bit");
    assert_eq!(bytes[5], 1, "fixture ELF is not little-endian");
    assert_eq!(u16_at(&bytes, 16), ET_DYN, "fixture is not ET_DYN");
    if !require_rela {
        return;
    }
    let table = usize::try_from(u64_at(&bytes, 32)).unwrap();
    let entry_size = usize::from(u16_at(&bytes, 54));
    let entry_count = usize::from(u16_at(&bytes, 56));
    assert!(entry_size >= 56, "ELF program-header entry is truncated");
    let dynamic = (0..entry_count)
        .map(|index| table + index * entry_size)
        .find(|offset| u32_at(&bytes, *offset) == PT_DYNAMIC)
        .expect("static PIE has no PT_DYNAMIC relocation metadata");
    let offset = usize::try_from(u64_at(&bytes, dynamic + 8)).unwrap();
    let size = usize::try_from(u64_at(&bytes, dynamic + 32)).unwrap();
    let entries = bytes[offset..offset + size]
        .chunks_exact(16)
        .map(|entry| (u64_at(entry, 0), u64_at(entry, 8)))
        .collect::<Vec<_>>();
    assert!(
        entries.iter().any(|entry| entry.0 == DT_RELA),
        "static PIE has no DT_RELA"
    );
    assert!(
        entries.iter().any(|entry| entry.0 == DT_RELASZ && entry.1 != 0),
        "static PIE has an empty relocation table"
    );
}

fn run(isa: &str, engine_name: &str, static_pie: bool) {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join(if static_pie { "static-pie" } else { "pie" });
    guest::pie_exec(isa, &executable, static_pie);
    assert_et_dyn(&executable, static_pie);
    let output = Command::new(engine::EngineBinaryPaths::required().named(engine_name))
        .args(["--guest-isa", isa])
        .arg(&executable)
        .env_remove("HL_AUTHORITY_FD")
        .env_remove("HL_AUTHORITY_HEALTH_FD")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{isa} {} failed: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        if static_pie { "static PIE" } else { "PIE" },
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pie-exec-ok\n");
}

#[test]
fn x86_64_pie() {
    run("x86_64", "hl-x86_64", false);
}

#[test]
fn x86_64_static_pie() {
    run("x86_64", "hl-x86_64", true);
}

#[test]
fn aarch64_pie() {
    run("aarch64", "hl-aarch64", false);
}

#[test]
fn aarch64_static_pie() {
    run("aarch64", "hl-aarch64", true);
}
