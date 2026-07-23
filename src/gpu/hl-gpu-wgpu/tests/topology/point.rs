use super::*;

#[test]
fn pointlist_lights_exact_pixels() {
    if WgpuExecutor::new(DeviceConfig::default()).is_err() {
        return;
    }
    // Scattered, well-separated centers (corners, mid-edges, an interior cluster) — a point that bleeds
    // to a neighbour or shifts by one is caught, and the wide gaps prove "between stays clear".
    let pts = [
        (2, 2),
        (29, 2),
        (2, 29),
        (29, 29),
        (16, 16),
        (10, 20),
        (23, 8),
        (16, 17),
    ];
    let verts: Vec<[f32; 2]> = pts.iter().map(|&(x, y)| center_ndc(x, y)).collect();
    let out = draw(Topology::PointList, &verts);

    let mut mask = empty_mask();
    for &(x, y) in &pts {
        set(&mut mask, x, y);
    }
    assert_exact_and_write("pointlist", &out, &mask);
}
