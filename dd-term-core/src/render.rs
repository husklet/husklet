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
    /// Encode as a PNG byte stream (see [`crate::png`]).
    pub fn to_png(&self) -> Vec<u8> {
        crate::png::encode_rgba(self.width, self.height, &self.rgba)
    }
    /// The pixel at `(x, y)` as `(r, g, b, a)`.
    pub fn pixel(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * self.width + x) * 4) as usize;
        (self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3])
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
        let mut rgba = vec![0u8; (width * height * 4) as usize];

        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                let cell = grid.cell(row, col).copied().unwrap_or_default();
                let is_cursor = grid.cursor_visible && row == grid.cursor_row && col == grid.cursor_col;
                self.draw_cell(&mut rgba, width, col as u32 * cw, row as u32 * ch, &cell, is_cursor);
            }
        }
        Image { width, height, rgba }
    }

    /// Convenience: rasterize + PNG-encode in one step.
    pub fn render_png(&self, grid: &Grid) -> Vec<u8> {
        self.render(grid).to_png()
    }

    fn draw_cell(&self, rgba: &mut [u8], img_w: u32, ox: u32, oy: u32, cell: &Cell, is_cursor: bool) {
        let (cw, ch) = self.cell_px();
        let mut fg = self.resolve(cell.fg, true, cell.attrs);
        let mut bg = self.resolve(cell.bg, false, cell.attrs);
        if cell.attrs.contains(Attrs::REVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        // A visible cursor draws as a filled block with inverted ink (classic block cursor).
        if is_cursor {
            std::mem::swap(&mut fg, &mut bg);
            bg = self.cursor;
        }
        // Background fill.
        for y in 0..ch {
            for x in 0..cw {
                put(rgba, img_w, ox + x, oy + y, bg);
            }
        }
        if cell.attrs.contains(Attrs::HIDDEN) {
            return;
        }
        // Foreground glyph, scaled.
        let bitmap = font::glyph(cell.ch);
        for (gy, rowbits) in bitmap.iter().enumerate() {
            for gx in 0..GLYPH_W {
                if rowbits & (0x80 >> gx) != 0 {
                    for sy in 0..self.scale {
                        for sx in 0..self.scale {
                            let px = ox + gx as u32 * self.scale + sx;
                            let py = oy + gy as u32 * self.scale + sy;
                            put(rgba, img_w, px, py, fg);
                        }
                    }
                }
            }
        }
    }

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
                ansi_256(i)
            }
        }
    }
}

fn put(rgba: &mut [u8], img_w: u32, x: u32, y: u32, (r, g, b): (u8, u8, u8)) {
    let i = ((y * img_w + x) * 4) as usize;
    rgba[i] = r;
    rgba[i + 1] = g;
    rgba[i + 2] = b;
    rgba[i + 3] = 0xff;
}

/// The xterm 256-color palette: 0..16 named, 16..232 the 6×6×6 cube, 232..256 grayscale ramp.
fn ansi_256(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), (0xcd, 0x00, 0x00), (0x00, 0xcd, 0x00), (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee), (0xcd, 0x00, 0xcd), (0x00, 0xcd, 0xcd), (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f), (0xff, 0x00, 0x00), (0x00, 0xff, 0x00), (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff), (0xff, 0x00, 0xff), (0x00, 0xff, 0xff), (0xff, 0xff, 0xff),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let steps = [0u8, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
            (steps[(n / 36 % 6) as usize], steps[(n / 6 % 6) as usize], steps[(n % 6) as usize])
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
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
        let red = ansi_256(1);
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
