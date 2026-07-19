//! CPU renderer: rasterize a [`Grid`] to an RGBA image (and PNG) using the embedded [`crate::font`].
//!
//! No GPU. This is (a) the headless render oracle — tests assert real pixels and can dump a `.png` to
//! eyeball a terminal frame with no display — and (b) the shared cell/color/glyph logic the GPU shell
//! reuses (it uploads this same font atlas). What is verified here is pixel-identical to what ships.

use crate::font::{self, GLYPH_H, GLYPH_W};
use crate::grid::{Attrs, Cell, Color, Grid};

/// An RGBA8 image (`w*h*4` bytes, row-major, top-to-bottom).
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// Encode as a PNG byte stream using uncompressed DEFLATE blocks.
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&self.width.to_be_bytes());
        ihdr.extend_from_slice(&self.height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        Self::write_chunk(&mut out, b"IHDR", &ihdr);

        let stride = (self.width * 4) as usize;
        let mut raw = Vec::with_capacity((stride + 1) * self.height as usize);
        for row in self.rgba.chunks_exact(stride) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        Self::write_chunk(&mut out, b"IDAT", &Self::zlib_stored(&raw));
        Self::write_chunk(&mut out, b"IEND", &[]);
        out
    }
    /// The pixel at `(x, y)` as `(r, g, b, a)`.
    pub fn pixel(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * self.width + x) * 4) as usize;
        (
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        )
    }

    fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&Self::crc32(&out[start..]).to_be_bytes());
    }

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 6 + data.len() / 0xffff * 5);
        out.extend_from_slice(&[0x78, 0x01]);
        for (index, chunk) in data.chunks(0xffff).enumerate() {
            let last = (index + 1) * 0xffff >= data.len();
            out.push(u8::from(last));
            let len = chunk.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        if data.is_empty() {
            out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
        }
        out.extend_from_slice(&Self::adler32(data).to_be_bytes());
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
}

/// Renderer configuration: integer scale (each font pixel becomes `scale × scale`) + the palette.
pub struct CpuRenderer {
    /// Each 8×8 glyph pixel is drawn as a `scale × scale` block, so cells are `8*scale` square.
    pub scale: u32,
    pub fg_default: (u8, u8, u8),
    pub bg_default: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
}

impl Default for CpuRenderer {
    fn default() -> Self {
        CpuRenderer {
            scale: 2,
            fg_default: (0xcc, 0xcc, 0xcc),
            bg_default: (0x14, 0x14, 0x14),
            cursor: (0xcc, 0xcc, 0xcc),
        }
    }
}

impl CpuRenderer {
    /// Pixel size of one cell.
    pub fn cell_px(&self) -> (u32, u32) {
        (GLYPH_W as u32 * self.scale, GLYPH_H as u32 * self.scale)
    }

    /// Rasterize `grid` to an [`Image`].
    pub fn render(&self, grid: &Grid) -> Image {
        let (cw, ch) = self.cell_px();
        let width = grid.cols() as u32 * cw;
        let height = grid.rows() as u32 * ch;
        let mut image = Image {
            width,
            height,
            rgba: vec![0u8; (width * height * 4) as usize],
        };

        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                let cell = grid.cell(row, col).copied().unwrap_or_default();
                let is_cursor =
                    grid.cursor_visible && row == grid.cursor_row && col == grid.cursor_col;
                image.draw_cell(
                    self,
                    width,
                    col as u32 * cw,
                    row as u32 * ch,
                    &cell,
                    is_cursor,
                );
            }
        }
        image
    }

    /// Convenience: rasterize + PNG-encode in one step.
    pub fn render_png(&self, grid: &Grid) -> Vec<u8> {
        self.render(grid).to_png()
    }
}

impl Image {
    fn draw_cell(
        &mut self,
        renderer: &CpuRenderer,
        img_w: u32,
        ox: u32,
        oy: u32,
        cell: &Cell,
        is_cursor: bool,
    ) {
        let (cw, ch) = renderer.cell_px();
        let mut fg = renderer.resolve(cell.fg, true, cell.attrs);
        let mut bg = renderer.resolve(cell.bg, false, cell.attrs);
        if cell.attrs.contains(Attrs::REVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        // A visible cursor draws as a filled block with inverted ink (classic block cursor).
        if is_cursor {
            std::mem::swap(&mut fg, &mut bg);
            bg = renderer.cursor;
        }
        self.fill_cell(img_w, ox, oy, cw, ch, bg);
        if cell.attrs.contains(Attrs::HIDDEN) {
            return;
        }
        self.draw_glyph(renderer, img_w, ox, oy, cell.ch, fg);
    }

    fn fill_cell(&mut self, img_w: u32, ox: u32, oy: u32, cw: u32, ch: u32, color: (u8, u8, u8)) {
        for y in 0..ch {
            for x in 0..cw {
                self.put(img_w, ox + x, oy + y, color);
            }
        }
    }

    fn draw_glyph(
        &mut self,
        renderer: &CpuRenderer,
        img_w: u32,
        ox: u32,
        oy: u32,
        ch: char,
        color: (u8, u8, u8),
    ) {
        let bitmap = font::EMBEDDED.lookup(ch);
        for (gy, rowbits) in bitmap.iter().enumerate() {
            for gx in 0..GLYPH_W {
                if rowbits & (0x80 >> gx) == 0 {
                    continue;
                }
                let x = ox + gx as u32 * renderer.scale;
                let y = oy + gy as u32 * renderer.scale;
                self.fill_cell(img_w, x, y, renderer.scale, renderer.scale, color);
            }
        }
    }

    fn put(&mut self, img_w: u32, x: u32, y: u32, (r, g, b): (u8, u8, u8)) {
        let i = ((y * img_w + x) * 4) as usize;
        self.rgba[i..i + 4].copy_from_slice(&[r, g, b, 0xff]);
    }
}

impl CpuRenderer {
    /// Resolve a [`Color`] to RGB. BOLD brightens the low 8 indexed colors (the common terminal
    /// convention). `is_fg` selects which default applies.
    fn resolve(&self, color: Color, is_fg: bool, attrs: Attrs) -> (u8, u8, u8) {
        match color {
            Color::Default => {
                if is_fg {
                    self.fg_default
                } else {
                    self.bg_default
                }
            }
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(mut i) => {
                if is_fg && attrs.contains(Attrs::BOLD) && i < 8 {
                    i += 8; // bold → bright variant
                }
                self.ansi(i)
            }
        }
    }

    /// Resolve an xterm 256-color palette index.
    fn ansi(&self, index: u8) -> (u8, u8, u8) {
        const BASE: [(u8, u8, u8); 16] = [
            (0x00, 0x00, 0x00),
            (0xcd, 0x00, 0x00),
            (0x00, 0xcd, 0x00),
            (0xcd, 0xcd, 0x00),
            (0x00, 0x00, 0xee),
            (0xcd, 0x00, 0xcd),
            (0x00, 0xcd, 0xcd),
            (0xe5, 0xe5, 0xe5),
            (0x7f, 0x7f, 0x7f),
            (0xff, 0x00, 0x00),
            (0x00, 0xff, 0x00),
            (0xff, 0xff, 0x00),
            (0x5c, 0x5c, 0xff),
            (0xff, 0x00, 0xff),
            (0x00, 0xff, 0xff),
            (0xff, 0xff, 0xff),
        ];
        match index {
            0..=15 => BASE[index as usize],
            16..=231 => {
                let cube = index - 16;
                let steps = [0u8, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
                (
                    steps[(cube / 36 % 6) as usize],
                    steps[(cube / 6 % 6) as usize],
                    steps[(cube % 6) as usize],
                )
            }
            _ => {
                let gray = 8 + (index - 232) * 10;
                (gray, gray, gray)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vt;

    #[test]
    fn renders_expected_dimensions() {
        let vt = Vt::new(10, 3);
        let img = CpuRenderer::default().render(vt.grid());
        assert_eq!(img.width, 10 * 8 * 2);
        assert_eq!(img.height, 3 * 8 * 2);
        assert_eq!(img.rgba.len() as u32, img.width * img.height * 4);
    }

    #[test]
    fn glyph_ink_and_background_pixels() {
        // Render a red 'A' on the default background; assert ink pixels are red and gaps are background.
        let mut vt = Vt::new(4, 1);
        vt.advance_bytes(b"\x1b[31mA");
        vt.advance_bytes(b"\x1b[?25l"); // hide the cursor so it doesn't invert cell 1
        let r = CpuRenderer::default();
        let img = r.render(vt.grid());
        // Some pixel inside cell (0,0) must be the ANSI red ink...
        let red = r.ansi(1);
        let mut found_ink = false;
        for y in 0..16 {
            for x in 0..16 {
                if img.pixel(x, y) == (red.0, red.1, red.2, 0xff) {
                    found_ink = true;
                }
            }
        }
        assert!(found_ink, "the red 'A' glyph should have red ink pixels");
        // ...and cell (0,1) is empty → all background.
        assert_eq!(img.pixel(16 + 4, 8), (0x14, 0x14, 0x14, 0xff));
    }

    #[test]
    fn png_round_trips_signature() {
        let vt = Vt::new(4, 2);
        let png = CpuRenderer::default().render_png(vt.grid());
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    }
}
