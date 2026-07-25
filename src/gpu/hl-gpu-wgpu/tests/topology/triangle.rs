use super::*;

// ---------------------------------------------------------------------------------------------------
// TriangleStrip vs TriangleList — 4 verts (2 triangles, shared edge) == the 6-vert list of the same quad
// ---------------------------------------------------------------------------------------------------

#[test]

fn trianglestrip_quad_equals_trianglelist() {
    if WgpuExecutor::new(DeviceConfig::default()).is_err() {
        return;
    }
    // Corners of an axis-aligned quad on pixel centers. Strip winding is (v0,v1,v2),(v1,v2,v3); with
    // cull off both triangles paint, filling the solid rect [tl..br). The equivalent TriangleList is the
    // very same two triangles as 6 explicit vertices.
    let tl = (6, 6);
    let tr = (25, 6);
    let bl = (6, 25);
    let br = (25, 25);
    let c = |p: (i32, i32)| center_ndc(p.0, p.1);

    // TriangleStrip: v0=TL, v1=TR, v2=BL, v3=BR.
    let strip = draw(Topology::TriangleStrip, &[c(tl), c(tr), c(bl), c(br)]);
    // TriangleList: the same two triangles (TL,TR,BL) + (TR,BL,BR).
    let list = draw(
        Topology::TriangleList,
        &[c(tl), c(tr), c(bl), c(tr), c(bl), c(br)],
    );

    // Both must equal the solid quad by the top-left fill rule: cols/rows 6..25.
    let mask = quad_mask(tl, br);
    assert_exact_and_write("trianglestrip_quad", &strip, &mask);
    assert_exact_and_write("trianglelist_quad", &list, &mask);

    // And they must be byte-for-byte identical to each other (strip and list rasterize the same coverage).
    assert_eq!(
        strip, list,
        "TriangleStrip and the equivalent TriangleList produced different pixels — a strip-winding or \
         fill-rule mismatch"
    );
}

// ---------------------------------------------------------------------------------------------------
// tiny built-in PNG encoder (RGBA8, stored DEFLATE) — human visual confirmation only
// ---------------------------------------------------------------------------------------------------
