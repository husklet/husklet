use super::*;

#[test]
fn present_returns_the_presented_pair() {
    // A well-formed present flows a Presentation{surface, texture} back out of execute.
    let (mut exec, mut res) = primed(&[
        Cmd::CreateTexture(
            1,
            tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::PRESENT),
        ),
        Cmd::CreateSurface(
            1,
            SurfaceDesc {
                width: 4,
                height: 4,
                format: TextureFormat::Bgra8Unorm,
                hlp_surface: 1,
            },
        ),
    ]);
    let presents = exec
        .execute(
            &mut res,
            &[Cmd::Present {
                surface: 1,
                texture: 1,
            }],
        )
        .unwrap();
    assert_eq!(presents.len(), 1);
    assert_eq!(
        (presents[0].surface, presents[0].texture),
        (hl_gpu::SurfaceId(1), TextureId(1))
    );
}
