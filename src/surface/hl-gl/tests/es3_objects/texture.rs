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

    // Bad levels / extent → GL_INVALID_VALUE; bad target → GL_INVALID_ENUM.
    let _ = bound_texture(&mut c);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 2, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
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
fn tex_image_3d_then_sub_image_3d_write_the_layer0_plane() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_3D, t);

    // A full 4x4x2 upload of a solid blue layer-0 plane.
    let blue = [0u8, 0, 255, 255].repeat(4 * 4);
    record::tex_image_3d(&mut c, GL_TEXTURE_3D, 0, 4, 4, 2, &blue);
    {
        let tex = c.textures.get(t).expect("3D texture allocated");
        assert_eq!((tex.w, tex.h), (4, 4));
        assert_eq!(&tex.data[0..4], &[0, 0, 255, 255], "texel (0,0) is blue");
    }

    // Overwrite the top-left 2x2 with red (zoffset 0, layer-0 plane).
    let red = [255u8, 0, 0, 255].repeat(2 * 2);
    record::tex_sub_image_3d(&mut c, GL_TEXTURE_3D, 0, 0, 0, 0, 2, 2, 1, &red);
    let tex = c.textures.get(t).unwrap();
    assert_eq!(
        &tex.data[0..4],
        &[255, 0, 0, 255],
        "sub-image overwrote texel (0,0) to red"
    );
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
