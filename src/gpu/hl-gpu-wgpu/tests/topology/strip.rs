use super::*;

// ---------------------------------------------------------------------------------------------------
// LineStrip — a connected chain (monotone right/down staircase) with shared vertices painted once
// ---------------------------------------------------------------------------------------------------

#[test]

fn linestrip_connected_polyline() {
    if WgpuExecutor::new(DeviceConfig::default()).is_err() {
        return;
    }
    // A 6-vertex strip tracing a staircase that only ever moves RIGHT or DOWN: down, right, down, right,
    // down. Five connected segments sharing four interior corners. Because the path is monotone, each
    // corner is the excluded (max) endpoint of the segment arriving at it and the kept (min) endpoint of
    // the segment leaving it, so the diamond-exit rule paints every corner exactly once and the polyline
    // is fully connected — no hole, no double-paint. A strip that restarts per pair (drawing disjoint
    // segments), reverses a segment, or double-paints a corner fails. Only the final vertex (28,29) is
    // dropped (it is nobody's min endpoint).
    let chain = [(3, 3), (3, 12), (14, 12), (14, 20), (28, 20), (28, 29)];
    let verts: Vec<[f32; 2]> = chain.iter().map(|&(x, y)| center_ndc(x, y)).collect();
    let out = draw(Topology::LineStrip, &verts);

    let mask = linestrip_mask(&chain);
    assert_exact_and_write("linestrip", &out, &mask);
}
