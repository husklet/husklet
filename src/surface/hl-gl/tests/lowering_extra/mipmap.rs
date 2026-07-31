use super::*;

// ---------------------------------------------------------------------------------------------------
// Mip levels: `glTexImage2D` must store its `level`, not redefine the base image with it
//
// `level` was ignored, so uploading a chain 8x8, 4x4, 2x2, 1x1 left the 1x1 image as the ONE level the
// texture had. Every draw sampling it read a flat colour, whatever its min filter — and it looked like
// "the smallest level is always sampled", because the smallest level's DATA was the base image.
// ---------------------------------------------------------------------------------------------------

/// Bind a texture and upload a full chain from `size` down to 1x1, level `k` filled with byte `0x10 + k`.
fn upload_chain(c: &mut GlContext, size: i32) -> u32 {
    let tex = c.textures.gen();
    record::bind_texture(c, GL_TEXTURE_2D, tex);
    let mut level = 0u32;
    let mut extent = size;
    loop {
        let data = vec![0x10u8 + level as u8; (extent * extent * 4) as usize];
        if level == 0 {
            record::tex_image_2d(c, extent, extent, &data);
        } else {
            record::tex_image_2d_level(c, level, extent, extent, &data);
        }
        if extent == 1 {
            break;
        }
        extent /= 2;
        level += 1;
    }
    tex
}

#[test]
fn a_mip_chain_keeps_its_base_image_and_every_level() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!((t.w, t.h), (8, 8), "the base image is level 0, not level 3");
    assert_eq!(t.data[0], 0x10, "the base image holds level 0's bytes");
    assert_eq!(t.mip_chain().len(), 3, "levels 1, 2 and 3 above the base");
    assert_eq!(t.mip_levels(), 4);
    let chain = t.mip_chain();
    assert_eq!((chain[0].w, chain[0].h, chain[0].data[0]), (4, 4, 0x11));
    assert_eq!((chain[1].w, chain[1].h, chain[1].data[0]), (2, 2, 0x12));
    assert_eq!((chain[2].w, chain[2].h, chain[2].data[0]), (1, 1, 0x13));
}

#[test]
fn a_single_level_texture_declares_one_level_and_no_chain() {
    let mut c = ctx();
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 8, 8, &[0x22u8; 8 * 8 * 4]);
    let t = c.textures.get(tex).expect("the texture");
    assert!(t.mip_chain().is_empty());
    assert_eq!(t.mip_levels(), 1, "the overwhelmingly common case is unchanged");
}

/// A chain with a gap, or one whose extents do not halve, is not a chain: the host texture must not
/// declare a level it cannot fill.
#[test]
fn a_broken_chain_stops_at_the_first_bad_level() {
    let mut c = ctx();
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 8, 8, &[0u8; 8 * 8 * 4]);
    record::tex_image_2d_level(&mut c, 1, 4, 4, &[0u8; 4 * 4 * 4]);
    // Level 2 should be 2x2; a 3x3 upload breaks the halving.
    record::tex_image_2d_level(&mut c, 2, 3, 3, &[0u8; 3 * 3 * 4]);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!(t.mip_chain().len(), 1, "only level 1 is a valid continuation");
    assert_eq!(t.mip_levels(), 2);
}

#[test]
fn max_level_clamps_the_declared_level_count() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_parameter(&mut c, GL_TEXTURE_MAX_LEVEL, 1);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!(t.mip_levels(), 2, "GL_TEXTURE_MAX_LEVEL = 1 means levels 0 and 1");
}

#[test]
fn generate_mipmap_derives_the_whole_chain_from_the_base() {
    let mut c = ctx();
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    // A uniform base image box-filters to the same value at every level, so the arithmetic is checkable
    // by hand: (200 + 200 + 200 + 200 + 2) / 4 = 200.
    record::tex_image_2d(&mut c, 4, 4, &[200u8; 4 * 4 * 4]);
    c.generate_mipmap(GL_TEXTURE_2D);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!(t.mip_chain().len(), 2, "4x4 -> 2x2 -> 1x1");
    assert_eq!(t.mip_levels(), 3);
    for level in t.mip_chain() {
        assert!(level.data.iter().all(|&b| b == 200));
    }
    // This used to validate and return, leaving one level behind.
    assert_eq!((t.mip_chain()[1].w, t.mip_chain()[1].h), (1, 1));
}

#[test]
fn generate_mipmap_averages_a_two_by_two_block() {
    let mut c = ctx();
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    // One 2x2 image whose red channel is 0, 100, 200, 255: (0 + 100 + 200 + 255 + 2) / 4 = 139.
    let mut data = vec![0u8; 2 * 2 * 4];
    for (index, value) in [0u8, 100, 200, 255].iter().enumerate() {
        data[index * 4] = *value;
    }
    record::tex_image_2d(&mut c, 2, 2, &data);
    c.generate_mipmap(GL_TEXTURE_2D);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!(t.mip_chain().len(), 1);
    assert_eq!(t.mip_chain()[0].data[0], 139);
}

// ---------------------------------------------------------------------------------------------------
// GL_TEXTURE_BASE_LEVEL / GL_TEXTURE_MAX_LEVEL select which levels the texture HAS
// ---------------------------------------------------------------------------------------------------

/// GL's base level RE-INDEXES the pyramid: with `GL_TEXTURE_BASE_LEVEL = 2` the texture *is* level 2 and
/// below, and levels 0 and 1 are not part of it at all. The differential caught this at a MAGNIFYING ratio,
/// where no LOD computation is involved and there is therefore no implementation latitude to appeal to:
/// the level sampled is exactly the base level, and a draw under `base = 2` returned level 0.
#[test]
fn base_level_rebases_the_pyramid_it_hands_the_host() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8); // levels 0..3 at 8, 4, 2, 1
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_parameter(&mut c, GL_TEXTURE_BASE_LEVEL, 2);
    let t = c.textures.get(tex).expect("the texture");

    let levels = t.effective_levels();
    assert_eq!(levels.len(), 2, "levels 2 and 3 remain");
    assert_eq!(
        (levels[0].0, levels[0].1),
        (2, 2),
        "the host's level 0 is GL's level 2"
    );
    assert_eq!(levels[0].2[0], 0x12, "and carries level 2's pixels");
    assert_eq!((levels[1].0, levels[1].1), (1, 1));
    assert_eq!(t.mip_levels(), 2);
}

#[test]
fn max_level_trims_the_bottom_of_the_window() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_parameter(&mut c, GL_TEXTURE_BASE_LEVEL, 1);
    record::tex_parameter(&mut c, GL_TEXTURE_MAX_LEVEL, 2);
    let t = c.textures.get(tex).expect("the texture");
    let levels = t.effective_levels();
    assert_eq!(levels.len(), 2, "levels 1 and 2 only");
    assert_eq!((levels[0].0, levels[0].2[0]), (4, 0x11));
    assert_eq!((levels[1].0, levels[1].2[0]), (2, 0x12));
}

/// Changing the window makes the resident host upload stale, so it must bump the generation — otherwise
/// the re-based pyramid is computed and never sent.
#[test]
fn changing_the_level_window_bumps_the_generation() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    let before = c.textures.get(tex).expect("the texture").gen;
    record::tex_parameter(&mut c, GL_TEXTURE_BASE_LEVEL, 2);
    let after = c.textures.get(tex).expect("the texture").gen;
    assert_ne!(before, after, "the resident upload is stale and must be re-sent");

    // A filter change is NOT a storage change and must leave the generation alone.
    let before = after;
    record::tex_parameter(&mut c, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    assert_eq!(
        c.textures.get(tex).expect("the texture").gen,
        before,
        "a sampler parameter must not invalidate the upload"
    );
}

/// The default window is the whole pyramid, so an ordinary texture is unchanged.
#[test]
fn the_default_window_is_every_level() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!(t.effective_levels().len(), 4);
    assert_eq!(t.effective_levels()[0].0, 8, "level 0 is still the base");
}

/// A base level past the levels that exist leaves the texture mipmap-incomplete. Keep level 0 rather than
/// handing the host an empty pyramid — a host texture must have at least one level.
#[test]
fn a_base_level_past_the_chain_falls_back_to_level_zero() {
    let mut c = ctx();
    let tex = upload_chain(&mut c, 8);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_parameter(&mut c, GL_TEXTURE_BASE_LEVEL, 9);
    let t = c.textures.get(tex).expect("the texture");
    assert_eq!(t.effective_levels().len(), 1);
    assert_eq!(t.effective_levels()[0].0, 8);
}
