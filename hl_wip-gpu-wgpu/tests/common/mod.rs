//! Shared plumbing for the exact-pixel render-conformance demo battery.
//!
//! Every demo in this suite mints IR directly (no shim, no guest), runs it on the `WgpuExecutor` (lavapipe
//! on this headless host), reads back a color target, and asserts EXACT pixels. This module holds the
//! boilerplate they all share: GLSL→SPIR-V lowering, little-endian float packing, a `Session` bound to the
//! executor's own limits, texture descriptors, and a tiny built-in uncompressed-PNG encoder so every visual
//! demo drops a `/tmp/hl-demo/<name>.png` a human can open and confront.
//!
//! Not every function is used by every demo binary (each test file compiles its own copy of this module),
//! so unused-symbol warnings are silenced here rather than per call site.
#![allow(dead_code)]

use hl_gpu::protocol::model::descriptor::{ColorTargetState, TextureDesc};
use hl_gpu::protocol::model::enums::{TextureDim, TextureFormat};
use hl_gpu::protocol::model::kernel::GlslDescriptor;
use hl_gpu::{FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::WgpuExecutor;

pub const OUT_DIR: &str = "/tmp/hl-demo";

/// Lower a GLSL stage to the SPIR-V-words payload `CreateShader { kind: Glsl }` consumes.
pub fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor { stage, entry: entry.to_string(), source: source.to_string() }.to_words()
}

/// Pack `f32`s little-endian — the std140/std430 uniform + storage payload byte layout.
pub fn le_f32(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// A `Session` bound to the executor's advertised limits, with `copy_alignment = 1` so the demos can drive
/// tightly-packed uploads/readbacks (matching the rest of the wgpu suite).
pub fn new_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

/// A single-mip 2D `Rgba8Unorm` texture descriptor with the given `usage`.
pub fn tex2d(w: u32, h: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// A 2D `Rgba8Unorm` texture with `mips` mip levels.
pub fn tex2d_mips(w: u32, h: u32, mips: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: mips,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// A 3D `Rgba8Unorm` texture with `depth` slices.
pub fn tex3d(w: u32, h: u32, depth: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D3,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// An opaque `Rgba8Unorm` color target with no blend and full write mask.
pub fn color_target() -> ColorTargetState {
    ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }
}

/// A texel at `(x, y)` in a tight-packed `w`-wide RGBA8 readback plane.
pub fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// `|a-b| <= tol` per channel — absorbs last-ULP unorm rounding only; the demos are all exact integers.
pub fn near_tol(a: [u8; 4], b: [u8; 4], tol: i16) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= tol)
}

/// `|a-b| <= 1` per channel.
pub fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    near_tol(a, b, 1)
}

// ---------------------------------------------------------------------------------------------------
// tiny built-in PNG encoder (RGBA8, STORED/uncompressed DEFLATE) — for human visual confirmation only
// ---------------------------------------------------------------------------------------------------

/// Write `rgba` (a tight `w*h*4` plane) to `/tmp/hl-demo/<name>.png`. Best-effort — a write failure only
/// warns, it never fails the demo (the exact-pixel assert is the real check).
pub fn write_png(name: &str, w: u32, h: u32, rgba: &[u8]) {
    let _ = std::fs::create_dir_all(OUT_DIR);
    let path = format!("{OUT_DIR}/{name}.png");
    let bytes = encode_png(w, h, rgba);
    if let Err(e) = std::fs::write(&path, &bytes) {
        eprintln!("warning: could not write {path}: {e}");
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b): (u32, u32) = (1, 0);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut pos = 0usize;
    while pos < raw.len() {
        let chunk = (raw.len() - pos).min(0xFFFF);
        let final_block = pos + chunk >= raw.len();
        out.push(if final_block { 1 } else { 0 });
        out.extend_from_slice(&(chunk as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
        out.extend_from_slice(&raw[pos..pos + chunk]);
        pos += chunk;
    }
    if raw.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0);
        let row = (y * w * 4) as usize;
        raw.extend_from_slice(&rgba[row..row + (w * 4) as usize]);
    }
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6);
    ihdr.extend_from_slice(&[0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut png, b"IEND", &[]);
    png
}
