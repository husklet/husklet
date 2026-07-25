// ============================ minimal, dependency-free PNG encoder ============================
//
// Truecolor+alpha (8-bit RGBA), a single IDAT of DEFLATE *stored* (uncompressed) blocks wrapped in a
// zlib stream. No external crate — the goal is a real, viewable .png as present evidence, not a small one.

/// Write `rgba` (`width*height*4`, top-left origin) as an 8-bit RGBA PNG.
pub fn write_png(
    path: &std::path::Path,
    width: i32,
    height: i32,
    rgba: &[u8],
) -> std::io::Result<()> {
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

    write_chunk(&mut out, b"IDAT", &Zlib::new(&raw).stored());
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
struct Zlib<'a> {
    data: &'a [u8],
}

impl<'a> Zlib<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn stored(&self) -> Vec<u8> {
        let data = self.data;
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
        out.extend_from_slice(&self.adler32().to_be_bytes());
        out
    }

    fn adler32(&self) -> u32 {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in self.data {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
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
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            self.value = c ^ (self.value >> 8);
        }
    }
    fn finish(self) -> u32 {
        self.value ^ 0xFFFF_FFFF
    }
}
