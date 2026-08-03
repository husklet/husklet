use super::*;

/// Bind a fresh texture to unit 0 and return its GL name.
fn bound_texture(c: &mut GlContext) -> u32 {
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(c, GL_TEXTURE_2D, t);
    t
}

#[test]
fn tex_storage_2d_sizes_and_seals_the_texture() {
    let mut c = ctx();
    let t = bound_texture(&mut c);

    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 64, 32);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    {
        let tex = c.textures.get(t).expect("texture exists");
        assert_eq!((tex.w, tex.h), (64, 32));
        assert!(tex.immutable, "glTexStorage2D makes the texture immutable");
        assert_eq!(
            tex.data.len(),
            64 * 32 * 4,
            "the RGBA8 base plane is allocated"
        );
    }

    // A second glTexStorage2D on an immutable texture is GL_INVALID_OPERATION.
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 16, 16);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // ES 3.0 §3.8.4: `levels` must be in `1..=floor(log2(max(w, h))) + 1`. An 8x8 texture therefore
    // accepts 1 through 4 (8, 4, 2, 1). This asserted that 2 was GL_INVALID_VALUE, which encoded the
    // driver's old "base level only" restriction — a large part of why immutable storage was unusable.
    let _ = bound_texture(&mut c);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 4, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR, "8x8 has four mip levels");
    let _ = bound_texture(&mut c);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 5, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE, "but not five");
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 0, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE, "nor zero");
    record::tex_storage_2d(&mut c, GL_TEXTURE_3D, 1, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

#[test]
fn tex_sub_image_2d_writes_into_allocated_storage() {
    let mut c = ctx();
    let t = bound_texture(&mut c);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 4, 4);

    // Overwrite the top-left 2x2 with red.
    let red = [255u8, 0, 0, 255].repeat(2 * 2);
    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, 0, 0, 2, 2, &red);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    let tex = c.textures.get(t).unwrap();
    assert_eq!(&tex.data[0..4], &[255, 0, 0, 255], "pixel (0,0) is red");

    // An out-of-bounds sub-rect is GL_INVALID_VALUE.
    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, 3, 3, 4, 4, &red);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn tex_sub_image_2d_keeps_canonical_rgba_for_native_bgra_storage() {
    let mut c = ctx();
    let t = bound_texture(&mut c);
    record::tex_image_2d_format(
        &mut c,
        1,
        1,
        &[0, 0, 0, 0xff],
        hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm,
    );

    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, 0, 0, 1, 1, &[0xff, 0, 0, 0xff]);

    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        c.textures.get(t).unwrap().data.as_slice(),
        [0xff, 0, 0, 0xff],
        "the CPU shadow stays canonical RGBA; native BGRA is render-target metadata"
    );
}

#[test]
fn native_bgra_copy_preserves_logical_channels() {
    let mut c = ctx();
    let source = c.textures.gen();
    let destination = c.textures.gen();
    let format = hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm;
    c.textures
        .image_2d(source, 1, 1, &[0xff, 0, 0, 0xff], format);
    c.textures
        .image_2d(destination, 1, 1, &[0, 0, 0, 0xff], format);

    assert!(c
        .textures
        .copy_sub(source, destination, 0, 0, 0, 0, 1, 1, false, false, false,));
    assert_eq!(
        c.textures.get(destination).unwrap().data.as_slice(),
        [0xff, 0, 0, 0xff],
        "copying between native-BGRA targets preserves canonical RGBA shadows"
    );
}

#[test]
fn tex_storage_3d_sizes_and_seals_the_array_texture() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D_ARRAY, t);

    record::tex_storage_3d(&mut c, GL_TEXTURE_2D_ARRAY, 1, 16, 8, 4);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    {
        let tex = c.textures.get(t).expect("array texture exists");
        assert_eq!((tex.w, tex.h), (16, 8), "layer-0 plane is sized to w*h");
        assert!(tex.immutable, "glTexStorage3D makes the texture immutable");
        assert_eq!(
            tex.data.len(),
            16 * 8 * 4,
            "the RGBA8 base plane is allocated"
        );
    }

    // Bad target → GL_INVALID_ENUM; bad extent → GL_INVALID_VALUE.
    let t2 = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, t2);
    record::tex_storage_3d(&mut c, GL_TEXTURE_2D, 1, 4, 4, 4);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    record::bind_texture(&mut c, GL_TEXTURE_3D, t2);
    record::tex_storage_3d(&mut c, GL_TEXTURE_3D, 0, 4, 4, 4);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn tex_image_3d_then_sub_image_3d_preserve_distinct_layers() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_3D, t);

    // A full 4x4x2 upload with distinct blue and green layers.
    let blue = [0u8, 0, 255, 255].repeat(4 * 4);
    let green = [0u8, 255, 0, 255].repeat(4 * 4);
    let volume = [blue.as_slice(), green.as_slice()].concat();
    record::tex_image_3d(&mut c, GL_TEXTURE_3D, 0, 4, 4, 2, &volume);
    {
        let tex = c.textures.get(t).expect("3D texture allocated");
        assert_eq!((tex.w, tex.h), (4, 4));
        assert_eq!(&tex.data[0..4], &[0, 0, 255, 255], "texel (0,0) is blue");
        assert_eq!(tex.depth, 2);
        assert_eq!(&tex.layer(1).unwrap()[0..4], &[0, 255, 0, 255]);
    }

    // Overwrite the top-left 2x2 with red (zoffset 0, layer-0 plane).
    let red = [255u8, 0, 0, 255].repeat(2 * 2);
    record::tex_sub_image_3d(&mut c, GL_TEXTURE_3D, 0, 0, 0, 1, 2, 2, 1, &red);
    let tex = c.textures.get(t).unwrap();
    assert_eq!(
        &tex.layer(1).unwrap()[0..4],
        &[255, 0, 0, 255],
        "sub-image overwrote layer 1 texel (0,0) to red"
    );
    assert_eq!(&tex.data[0..4], &[0, 0, 255, 255]);

    // Client-memory mip definitions and sub-updates retain every depth slice independently.
    let mip_blue = [1u8, 2, 3, 255].repeat(2 * 2);
    let mip_green = [4u8, 5, 6, 255].repeat(2 * 2);
    record::tex_image_3d(
        &mut c,
        GL_TEXTURE_3D,
        1,
        2,
        2,
        2,
        &[mip_blue.as_slice(), mip_green.as_slice()].concat(),
    );
    let mip_red = [9u8, 8, 7, 255].repeat(2 * 2);
    record::tex_sub_image_3d(&mut c, GL_TEXTURE_3D, 1, 0, 0, 1, 2, 2, 1, &mip_red);
    let mip = &c.textures.get(t).unwrap().mips[0];
    assert_eq!((mip.w, mip.h, mip.depth), (2, 2, 2));
    assert_eq!(&mip.data[..4], &[1, 2, 3, 255]);
    assert_eq!(&mip.layer(1).unwrap()[..4], &[9, 8, 7, 255]);
}

// ---- glGetTexParameteriv: filter/wrap state of the bound texture round-trips ---------------------

#[test]
fn get_tex_parameteriv_reports_bound_texture_filter_and_wrap() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_image_2d(&mut c, 2, 2, &[0u8; 16]);

    record::tex_parameter(&mut c, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    record::tex_parameter(&mut c, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    assert_eq!(
        query::get_tex_parameteriv(&c, GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER),
        GL_NEAREST as i32
    );
    assert_eq!(
        query::get_tex_parameteriv(&c, GL_TEXTURE_2D, GL_TEXTURE_WRAP_S),
        GL_CLAMP_TO_EDGE as i32
    );
    // MAG filter defaults to LINEAR; an unknown target reads 0.
    assert_eq!(
        query::get_tex_parameteriv(&c, GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER),
        GL_LINEAR as i32
    );
    assert_eq!(
        query::get_tex_parameteriv(&c, GL_TEXTURE_3D, GL_TEXTURE_MIN_FILTER),
        0
    );
}

#[test]
fn texture_swizzle_scalar_and_vector_forms_share_one_object_state() {
    let mut context = ctx();
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);

    record::tex_parameter(&mut context, GL_TEXTURE_SWIZZLE_R, GL_ONE);
    record::tex_parameter_vector(
        &mut context,
        GL_TEXTURE_SWIZZLE_RGBA,
        &[GL_ONE, GL_ONE, GL_ONE, GL_RED],
    );

    let expected = [GL_ONE, GL_ONE, GL_ONE, GL_RED];
    assert_eq!(context.textures.get(texture).unwrap().swizzle, expected);
    assert_eq!(
        query::get_tex_parameteriv(&context, GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_A),
        GL_RED as i32
    );
}
