//! A tiny, dependency-free PNG encoder (RGBA, 8-bit, uncompressed DEFLATE).
//!
//! Just enough to turn the CPU renderer's pixel buffer into a real `.png` file for headless render
//! assertions and self-inspection — no `image`/`png`/`flate2` crates (this host can't fetch them). Uses
//! stored (uncompressed) zlib blocks, so files are large but valid and any PNG viewer opens them.

/// Encode an RGBA8 buffer (`w*h*4` bytes, row-major, top-to-bottom) as a PNG byte stream.
pub fn encode_rgba(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(
        rgba.len(),
        (w as usize) * (h as usize) * 4,
        "rgba length must be w*h*4"
    );
    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]); // signature

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), no compression/filter/interlace.
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Raw image data = each row prefixed with a filter byte (0 = None).
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    let stride = (w * 4) as usize;
    for row in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[row * stride..row * stride + stride]);
    }
    let idat = zlib_stored(&raw);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// One PNG chunk: length (BE) + type + data + CRC32(type+data) (BE).
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// zlib stream wrapping DEFLATE stored (uncompressed) blocks: `0x78 0x01` + blocks + adler32(BE).
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF=0x78 (32K window, deflate), FLG=0x01 (no dict, check ok)
    let mut i = 0;
    while i < data.len() || (data.is_empty() && i == 0) {
        let chunk = &data[i..(i + 0xffff).min(data.len())];
        let last = i + chunk.len() >= data.len();
        out.push(if last { 1 } else { 0 }); // BFINAL bit, BTYPE=00 (stored)
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
        i += chunk.len().max(1);
        if data.is_empty() {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_signature_and_chunks() {
        // A 2x1 red/green image.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let png = encode_rgba(2, 1, &rgba);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // IHDR chunk type follows the 8-byte sig + 4-byte length.
        assert_eq!(&png[12..16], b"IHDR");
        // Width/height are the first 8 bytes of IHDR data.
        assert_eq!(&png[16..20], &2u32.to_be_bytes());
        assert_eq!(&png[20..24], &1u32.to_be_bytes());
        // Must contain IDAT and end with IEND + its CRC.
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn crc_and_adler_known_values() {
        // CRC32 of "IEND" is the canonical 0xAE426082.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        // Adler32 of "abc" is 0x024D0127.
        assert_eq!(adler32(b"abc"), 0x024D_0127);
    }
}
