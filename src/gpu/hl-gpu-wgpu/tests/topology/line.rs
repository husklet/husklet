use super::*;

// ---------------------------------------------------------------------------------------------------
// LineList — independent pairs: a horizontal, a vertical, and a 45° diagonal line
// ---------------------------------------------------------------------------------------------------

#[test]

fn linelist_axis_and_diagonal() {
    if WgpuExecutor::new(DeviceConfig::default()).is_err() {
        return;
    }
    // Three disjoint segments in ONE LineList draw (6 verts / 3 pairs). Each is axis-aligned or exact-45°
    // so its rasterized staircase is unambiguous; they don't touch, so each is asserted in isolation.
    //   horizontal: (3,5)   -> (27,5)      row 5, cols 3..=26   (27 excluded by diamond-exit)
    //   vertical:   (6,8)   -> (6,28)       col 6, rows 8..=27
    //   diagonal:   (12,10) -> (26,24)      perfect 45°, (12,10)..=(25,23)
    let segs: [((i32, i32), (i32, i32)); 3] =
        [((3, 5), (27, 5)), ((6, 8), (6, 28)), ((12, 10), (26, 24))];
    let mut verts = Vec::new();
    for (a, b) in segs {
        verts.push(center_ndc(a.0, a.1));
        verts.push(center_ndc(b.0, b.1));
    }
    let out = draw(Topology::LineList, &verts);

    let model: Vec<(i32, i32)> = segs.iter().flat_map(|&(a, b)| [a, b]).collect();
    let mask = linelist_mask(&model);
    assert_exact_and_write("linelist", &out, &mask);
}
