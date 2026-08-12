#![cfg(all(target_os = "linux", target_arch = "aarch64"))]

use sha2::Digest as _;
use std::{fs, process::Command};

const LINK_BASE: u64 = 0x40_0000;
const ENTRY_OFFSET: usize = 0x180;

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn static_aarch64(status: u16) -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, 183);
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 1);
    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 80, LINK_BASE);
    put_u64(&mut bytes, 88, LINK_BASE);
    let image_length = bytes.len() as u64;
    put_u64(&mut bytes, 96, image_length);
    put_u64(&mut bytes, 104, image_length);
    put_u64(&mut bytes, 112, 4096);
    put_u32(&mut bytes, ENTRY_OFFSET, 0xd280_0000 | u32::from(status) << 5);
    put_u32(&mut bytes, ENTRY_OFFSET + 4, 0xd280_0ba8);
    put_u32(&mut bytes, ENTRY_OFFSET + 8, 0xd400_0001);
    bytes
}

fn static_x86_64(status: u8) -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, 62);
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 1);
    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 80, LINK_BASE);
    put_u64(&mut bytes, 88, LINK_BASE);
    let image_length = bytes.len() as u64;
    put_u64(&mut bytes, 96, image_length);
    put_u64(&mut bytes, 104, image_length);
    put_u64(&mut bytes, 112, 4096);
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + 5].copy_from_slice(&[0xb8, 60, 0, 0, 0]);
    bytes[ENTRY_OFFSET + 5] = 0xbf;
    bytes[ENTRY_OFFSET + 6..ENTRY_OFFSET + 10].copy_from_slice(&u32::from(status).to_le_bytes());
    bytes[ENTRY_OFFSET + 10..ENTRY_OFFSET + 12].copy_from_slice(&[0x0f, 0x05]);
    bytes
}

#[test]
fn direct_aarch64_worker_defaults_to_retained_c() {
    let path = std::env::temp_dir().join(format!("hl-retained-direct-{}", std::process::id()));
    fs::write(&path, static_aarch64(42)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-aarch64"))
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn direct_x86_64_worker_defaults_to_retained_c() {
    let path = std::env::temp_dir().join(format!("hl-retained-direct-x86-{}", std::process::id()));
    fs::write(&path, static_x86_64(43)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(output.status.code(), Some(43));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn receipt_is_machine_readable_and_hash_bound() {
    let binary = env!("CARGO_BIN_EXE_hl-aarch64");
    let output = Command::new(binary).arg("--backend-receipt").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["schema"], "husklet-engine-backend-v1");
    assert_eq!(receipt["backend"], "retained-c");
    let expected = sha2::Sha256::digest(fs::read(binary).unwrap());
    let expected = expected.iter().fold(String::new(), |mut output, byte| {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
        output
    });
    assert_eq!(receipt["engine_sha256"], expected);
}

#[test]
fn receipt_accepts_both_guests_and_rejects_retired_selector_arguments() {
    let aarch64 = env!("CARGO_BIN_EXE_hl-aarch64");
    let rejected = Command::new(aarch64)
        .args(["--backend-receipt", "--engine-option", "HL_EXECUTION_BACKEND=c"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(125));
    assert!(rejected.stdout.is_empty());

    let x86 = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .arg("--backend-receipt")
        .output()
        .unwrap();
    assert!(x86.status.success());
    assert!(x86.stderr.is_empty());
    let receipt: serde_json::Value = serde_json::from_slice(&x86.stdout).unwrap();
    assert_eq!(receipt["backend"], "retained-c");

    let unsupported_guest = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args(["--backend-receipt", "--guest-isa", "riscv64"])
        .output()
        .unwrap();
    assert_eq!(unsupported_guest.status.code(), Some(125));
    assert!(unsupported_guest.stdout.is_empty());
    assert!(unsupported_guest.stderr.is_empty());
}
