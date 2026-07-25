use super::*;

pub(super) const GRID: [[f32; 4]; 4] = [
    [-0.5, 0.5, 0.25, 0.25],  // instance 0: top-left    (RED)
    [0.5, 0.5, 0.25, 0.25],   // instance 1: top-right   (GREEN)
    [-0.5, -0.5, 0.25, 0.25], // instance 2: bottom-left (BLUE)
    [0.5, -0.5, 0.25, 0.25],  // instance 3: bottom-right(WHITE)
];
const GRID_COLORS: [[u8; 4]; 4] = [RED, GREEN, BLUE, WHITE];

/// The exact expected coverage of one grid instance (its own cell) as a framebuffer mask.
fn grid_cell_mask(inst: usize) -> Vec<bool> {
    let [cx, cy, hx, hy] = GRID[inst];
    covered_mask(&[ndc_to_fb(cx - hx, cx + hx, cy - hy, cy + hy)])
}

/// Assert a rendered grid: EACH pixel must equal the color of whichever cell covers it, else CLEAR. This is
/// stricter than "4 colored blobs exist" — a quad that leaks outside its cell, or lands in another cell,
/// fails. Writes the combined PNG.
pub(super) fn assert_grid(name: &str, px: &[u8]) {
    write_png(name, px);
    let masks: Vec<Vec<bool>> = (0..4).map(grid_cell_mask).collect();

    let approx = |a: &[u8], b: [u8; 4]| {
        (a[0] as i16 - b[0] as i16).abs() <= 2
            && (a[1] as i16 - b[1] as i16).abs() <= 2
            && (a[2] as i16 - b[2] as i16).abs() <= 2
            && (a[3] as i16 - b[3] as i16).abs() <= 2
    };

    let mut bad = 0usize;
    let mut first: Option<(u32, u32, [u8; 4], [u8; 4])> = None;
    for i in 0..(W * H) as usize {
        let mut want = CLEAR;
        for c in 0..4 {
            if masks[c][i] {
                want = GRID_COLORS[c];
                break;
            }
        }
        let p = &px[i * 4..i * 4 + 4];
        if !approx(p, want) {
            bad += 1;
            if first.is_none() {
                first = Some((
                    (i as u32) % W,
                    (i as u32) / W,
                    [p[0], p[1], p[2], p[3]],
                    want,
                ));
            }
        }
    }
    if bad != 0 {
        eprintln!("=== grid demo `{name}` FAILED: {bad} wrong pixels ===");
        eprintln!("--- ACTUAL (bright=#) ---\n{}", ascii_actual(px));
        let (x, y, got, want) = first.unwrap();
        panic!(
            "grid demo `{name}`: {bad} pixels wrong; first at ({x},{y}) got {got:?} want {want:?} \
             (PNG at {OUT_DIR}/{name}.png) — a quad is huge, collapsed, or in the wrong cell"
        );
    }
    // Also prove every cell actually painted (guard against 'all clear happens to match nothing').
    for c in 0..4 {
        let painted = (0..(W * H) as usize)
            .filter(|&i| masks[c][i])
            .all(|i| approx(&px[i * 4..i * 4 + 4], GRID_COLORS[c]));
        assert!(
            painted,
            "grid demo `{name}`: instance {c}'s cell is not fully its color {:?}",
            GRID_COLORS[c]
        );
    }
    eprintln!("demo `{name}`: exact 2×2-grid match OK — PNG at {OUT_DIR}/{name}.png");
}

pub(super) const FLAT_COLOR_FS: &str = r#"#version 460
layout(location = 0) flat in vec4 vColor;
layout(location = 0) out vec4 o;
void main() { o = vColor; }
"#;
