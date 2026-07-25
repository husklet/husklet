use super::*;

// ==================================================================================================
// 1. texture_object — cudaTextureObject over a 2D array; POINT (exact texel) + LINEAR (exact midpoint).
// ==================================================================================================

#[test]
fn texture_object_point_and_linear_fetch_exact() {
    // 4x3 f32 array with distinct texels: T[row][col] = row*100 + col*10.
    let (w, h) = (4u32, 3u32);
    let texels: Vec<f32> = (0..h)
        .flat_map(|r| (0..w).map(move |c| (r as f32) * 100.0 + (c as f32) * 10.0))
        .collect();
    let t = |r: usize, c: usize| texels[r * w as usize + c];

    let mut array = CudaArray::new(w, h).unwrap();
    array.upload(&texels).unwrap();

    // ---- POINT filter: nearest texel, no interpolation ----
    let point_tex = TextureObject::from_array(&array, SamplerDesc::point_clamp());
    assert_eq!(point_tex.desc.filter, FilterMode::Point);
    // (1.5, 0.5) → floor → col 1, row 0 → exactly T[0][1] = 10.0.
    assert_eq!(texture::tex2d(&point_tex, 1.5, 0.5), t(0, 1));
    assert_eq!(texture::tex2d(&point_tex, 1.5, 0.5), 10.0);
    // (3.9, 2.1) → col 3, row 2 → T[2][3] = 230.0.
    assert_eq!(texture::tex2d(&point_tex, 3.9, 2.1), t(2, 3));
    assert_eq!(texture::tex2d(&point_tex, 3.9, 2.1), 230.0);

    // ---- LINEAR filter: bilinear interpolation ----
    let lin_tex = TextureObject::from_array(&array, SamplerDesc::linear_clamp());
    // Horizontal midpoint (2.0, 0.5): xb=1.5 → i0=1, a=0.5; yb=0.0 → b=0 → 0.5·T[0][1] + 0.5·T[0][2].
    let hmid = texture::tex2d(&lin_tex, 2.0, 0.5);
    assert_eq!(hmid, 0.5 * t(0, 1) + 0.5 * t(0, 2));
    assert_eq!(hmid, 15.0, "exact horizontal midpoint of 10 and 20");
    // Vertical midpoint (0.5, 1.0): xb=0 → a=0; yb=0.5 → j0=0, b=0.5 → 0.5·T[0][0] + 0.5·T[1][0].
    let vmid = texture::tex2d(&lin_tex, 0.5, 1.0);
    assert_eq!(vmid, 0.5 * t(0, 0) + 0.5 * t(1, 0));
    assert_eq!(vmid, 50.0, "exact vertical midpoint of 0 and 100");
    // Center-of-4 (2.0, 1.0): quarter-weight of the four surrounding texels.
    let cmid = texture::tex2d(&lin_tex, 2.0, 1.0);
    let want_center = 0.25 * (t(0, 1) + t(0, 2) + t(1, 1) + t(1, 2));
    assert_eq!(cmid, want_center);
    assert_eq!(cmid, 65.0, "exact center of {{10,20,110,120}}");
    // Sampling exactly ON a texel center (i+0.5, j+0.5) returns that texel unchanged (weights collapse).
    assert_eq!(texture::tex2d(&lin_tex, 2.5, 1.5), t(1, 2));
    assert_eq!(texture::tex2d(&lin_tex, 2.5, 1.5), 120.0);
}
