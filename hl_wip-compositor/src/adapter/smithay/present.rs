//! [`PngPresenter`]: the headless [`Presenter`] the Smithay adapter drives.
//!
//! The neutral [`Presenter`] port carries only geometry — no pixels — so this presenter keeps a small
//! side store of the actual client pixels the adapter deposits at commit time (keyed by [`SurfaceId`]).
//! When the scene composes a frame and calls [`Presenter::present`] for the base layer, the presenter
//! looks up that surface's deposited pixels, records a [`CapturedFrame`], and (optionally) writes a real
//! `.png` to disk. A test reads the captured frames back through the shared `captures` handle and asserts
//! the client's pixels made it all the way through wl → scene → present, fully headless.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::scene::model::{OutputId, PresentableImage, Rect, SurfaceId, Visibility};
use crate::scene::port::{PresentTiming, PresentationFeedback, Presenter};

/// Client pixels deposited by the adapter, unpacked to tight top-left RGBA8888.
#[derive(Clone, Debug)]
pub struct StoredBuffer {
    pub width: i32,
    pub height: i32,
    /// Tight `width*height*4` RGBA, row-major, top-left origin.
    pub rgba: Vec<u8>,
}

/// A frame the presenter actually presented — the evidence a headless test asserts on.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedFrame {
    pub output: OutputId,
    pub surface: SurfaceId,
    pub width: i32,
    pub height: i32,
    /// Root-space top-left `(x, y)` this layer's content was composited at this cycle — the placement a
    /// popup/subsurface was routed to. Derived from the compose damage the scene handed `present`
    /// (`layer_damage` translates a layer's rect into root space by its offset), so a popup at a resolved
    /// positioner geometry, or a subsurface at `parent + set_position`, reports that offset here. `(0, 0)`
    /// when the layer contributed no damage this cycle (a clean base layer re-presented under a child).
    pub x: i32,
    pub y: i32,
    /// Tight `width*height*4` RGBA of the presented surface.
    pub rgba: Vec<u8>,
    pub serial: u64,
}

impl CapturedFrame {
    /// RGBA of the pixel at `(x, y)`, or `None` if out of bounds.
    pub fn pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]])
    }
}

/// A headless [`Presenter`] that captures composed frames (and optionally writes PNGs).
pub struct PngPresenter {
    /// Client pixels deposited by the adapter at commit, keyed by surface.
    store: HashMap<SurfaceId, StoredBuffer>,
    /// Frames presented, shared so a test thread can read them while the compositor thread writes.
    captures: Arc<Mutex<Vec<CapturedFrame>>>,
    /// If set, each presented frame is also written to `<dir>/frame-<serial>.png`.
    out_dir: Option<PathBuf>,
    serial: u64,
}

impl PngPresenter {
    /// A presenter that only captures frames in memory.
    pub fn new() -> PngPresenter {
        PngPresenter { store: HashMap::new(), captures: Arc::new(Mutex::new(Vec::new())), out_dir: None, serial: 0 }
    }

    /// A presenter that also writes each presented frame to a PNG under `dir`.
    pub fn with_png_dir(dir: impl Into<PathBuf>) -> PngPresenter {
        PngPresenter { out_dir: Some(dir.into()), ..PngPresenter::new() }
    }

    /// A clonable handle onto the captured-frame log — grab this BEFORE moving the presenter into the
    /// compositor thread, then read presented frames back from the test thread.
    pub fn captures(&self) -> Arc<Mutex<Vec<CapturedFrame>>> {
        Arc::clone(&self.captures)
    }

    /// Deposit a surface's just-committed client pixels. The adapter calls this from its commit handler
    /// immediately before driving the scene, so the following `present` can capture real pixels.
    pub fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer) {
        self.store.insert(surface, buffer);
    }

    /// Forget a surface's pixels (on detach / destroy).
    pub fn forget(&mut self, surface: SurfaceId) {
        self.store.remove(&surface);
    }
}

impl Default for PngPresenter {
    fn default() -> PngPresenter {
        PngPresenter::new()
    }
}

impl Presenter for PngPresenter {
    fn present(
        &mut self,
        output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentationFeedback {
        let Some(buf) = self.store.get(&image.surface) else {
            // No pixels were deposited for this surface — nothing to capture. Report offscreen so the
            // scene does not advance pacing as if a real frame shipped.
            return PresentationFeedback::offscreen();
        };
        self.serial += 1;
        let serial = self.serial;
        // Where this layer landed in root space: the top-left of its compose damage (which
        // `service/compose::layer_damage` produced by translating the layer rect by its root offset). A
        // clean layer carries no damage, so it reports `(0, 0)` — the base root's own origin.
        let (x, y) = damage
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| (r.x, r.y))
            .next()
            .unwrap_or((0, 0));
        let frame = CapturedFrame {
            output,
            surface: image.surface,
            width: buf.width,
            height: buf.height,
            x,
            y,
            rgba: buf.rgba.clone(),
            serial,
        };
        if let Some(dir) = &self.out_dir {
            let _ = std::fs::create_dir_all(dir);
            let path = dir.join(format!("frame-{serial}.png"));
            let _ = write_png(&path, frame.width, frame.height, &frame.rgba);
        }
        self.captures.lock().unwrap().push(frame);
        PresentationFeedback::delivered(
            serial,
            Some(PresentTiming { present_ns: timing.present_ns, refresh_ns: timing.refresh_ns, vsync: false }),
        )
    }

    fn set_visibility(&mut self, _surface: SurfaceId, _visibility: Visibility) {}
}

// ============================ minimal, dependency-free PNG encoder ============================
//
// Truecolor+alpha (8-bit RGBA), a single IDAT of DEFLATE *stored* (uncompressed) blocks wrapped in a
// zlib stream. No external crate — the goal is a real, viewable .png as present evidence, not a small one.

/// Write `rgba` (`width*height*4`, top-left origin) as an 8-bit RGBA PNG.
pub fn write_png(path: &std::path::Path, width: i32, height: i32, rgba: &[u8]) -> std::io::Result<()> {
    let bytes = encode_png(width as u32, height as u32, rgba);
    std::fs::write(path, bytes)
}

/// Encode an 8-bit RGBA PNG into a byte vector.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: truecolor + alpha
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Raw filtered image data: each scanline prefixed with filter byte 0 (None).
    let row_bytes = (width * 4) as usize;
    let mut raw = Vec::with_capacity((row_bytes + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        let start = y * row_bytes;
        raw.extend_from_slice(&rgba[start..start + row_bytes]);
    }

    write_chunk(&mut out, b"IDAT", &zlib_store(&raw));
    write_chunk(&mut out, b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// Wrap `data` in a zlib stream using DEFLATE stored (uncompressed) blocks.
fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78); // CMF: deflate, 32K window
    out.push(0x01); // FLG: no dict, check bits
    let mut i = 0;
    while i < data.len() || data.is_empty() {
        let remaining = data.len() - i;
        let block = remaining.min(0xFFFF);
        let final_block = i + block >= data.len();
        out.push(if final_block { 1 } else { 0 });
        let len = block as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&data[i..i + block]);
        i += block;
        if final_block {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

struct Crc32 {
    value: u32,
}

impl Crc32 {
    fn new() -> Crc32 {
        Crc32 { value: 0xFFFF_FFFF }
    }
    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let mut c = (self.value ^ byte as u32) & 0xFF;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            self.value = c ^ (self.value >> 8);
        }
    }
    fn finish(self) -> u32 {
        self.value ^ 0xFFFF_FFFF
    }
}
