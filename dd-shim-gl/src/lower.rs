//! Present-independent resource lowering: turn a tracked GL buffer/texture into the exact
//! `dd_gpu::ir::Cmd` sequence `gl_shim.c` emits for it at swap. These are pure functions (state in,
//! `Cmd`s out) — they do NOT drive the present/draw path (owned elsewhere for now); they are the
//! building blocks the future swap path will call, landed and tested early so the cutover is
//! byte-equivalent.
//!
//! Byte-parity with gl_shim.c is proven two ways in the tests: the produced `Cmd`s (a) round-trip
//! through the host decoder, and (b) encode to the *identical bytes* gl_shim.c hand-emits (built here
//! with the same `wire::Encoder` primitives the C shim mirrors).

use dd_shim_common::ir::{buffer_usage, texture_usage, BufferDesc, Cmd, SamplerDesc, TextureDesc};
use dd_shim_common::ir::{AddressMode, Filter, TextureDim, TextureFormat};

use crate::glconst::*;
use crate::state::Texture;
use crate::wireenc::{sampler_ir_id, stage_ir_id, tex_ir_id};

/// Lower a buffer's bytes to `CreateBuffer` + `WriteBuffer`, matching gl_shim.c:
/// `iu8(1) iu32(id) iu64(size) iu32(usage) istr("")` then `iu8(3) iu32(id) iu64(0) ibytes(data)`.
pub fn buffer_to_cmds(id: u32, usage: u32, data: &[u8]) -> [Cmd; 2] {
    [
        Cmd::CreateBuffer(id, BufferDesc { size: data.len() as u64, usage, label: String::new() }),
        Cmd::WriteBuffer { id, offset: 0, data: data.to_vec() },
    ]
}

/// A vertex VBO's residency upload (`buffer_usage::VERTEX`).
pub fn vertex_buffer_cmds(id: u32, data: &[u8]) -> [Cmd; 2] {
    buffer_to_cmds(id, buffer_usage::VERTEX, data)
}

/// An index/element buffer's residency upload (`buffer_usage::INDEX`).
pub fn index_buffer_cmds(id: u32, data: &[u8]) -> [Cmd; 2] {
    buffer_to_cmds(id, buffer_usage::INDEX, data)
}

/// gl_shim.c sampler min-filter code: Linear (1) for LINEAR / LINEAR_MIPMAP_*, else Nearest (0).
fn min_filter(gl: u32) -> Filter {
    match gl {
        GL_LINEAR | GL_LINEAR_MIPMAP_NEAREST | GL_LINEAR_MIPMAP_LINEAR => Filter::Linear,
        _ => Filter::Nearest,
    }
}
/// gl_shim.c mag-filter code: Linear (1) only for exactly GL_LINEAR, else Nearest (0).
fn mag_filter(gl: u32) -> Filter {
    if gl == GL_LINEAR {
        Filter::Linear
    } else {
        Filter::Nearest
    }
}
/// gl_shim.c wrap→address code: ClampToEdge(0) / MirrorRepeat(2) / else Repeat(1).
fn address_mode(gl: u32) -> AddressMode {
    match gl {
        GL_CLAMP_TO_EDGE => AddressMode::ClampToEdge,
        GL_MIRRORED_REPEAT => AddressMode::MirrorRepeat,
        _ => AddressMode::Repeat,
    }
}

/// The `CreateTexture` for a GL texture, matching gl_shim.c:
/// `{w,h,depth=1,mips=1,samples=1,dim=D2,fmt=Rgba8Unorm,usage=SAMPLED|RENDER_TARGET|COPY_DST}`.
pub fn create_texture_cmd(gl_tex: u32, t: &Texture) -> Cmd {
    Cmd::CreateTexture(
        tex_ir_id(gl_tex),
        TextureDesc {
            width: t.w as u32,
            height: t.h as u32,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET | texture_usage::COPY_DST,
            label: String::new(),
        },
    )
}

/// The `CreateSampler` for a GL texture's filter/wrap state, matching gl_shim.c's
/// `min,mag,mip=Nearest(0),addrU,addrV,addrW=ClampToEdge(0)`.
pub fn create_sampler_cmd(gl_tex: u32, t: &Texture) -> Cmd {
    Cmd::CreateSampler(
        sampler_ir_id(gl_tex),
        SamplerDesc {
            min_filter: min_filter(t.minf),
            mag_filter: mag_filter(t.magf),
            mip_filter: Filter::Nearest,
            address_u: address_mode(t.ws),
            address_v: address_mode(t.wt),
            address_w: AddressMode::ClampToEdge,
        },
    )
}

/// The staging upload for a texture's RGBA8 pixels: a `COPY_SRC` buffer at `stage_ir_id` + its write,
/// matching gl_shim.c's `iu8(1) iu32(stg) iu64(size) iu32(16) istr("")` + `iu8(3) ...`.
pub fn texture_staging_cmds(gl_tex: u32, t: &Texture) -> [Cmd; 2] {
    buffer_to_cmds(stage_ir_id(gl_tex), buffer_usage::COPY_SRC, &t.data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_shim_common::ir::{encode_stream, Cmd};
    use dd_shim_common::wire::{Decoder, Encoder};

    // Rebuild the exact bytes gl_shim.c hand-emits for a resource, using the same wire primitives, and
    // assert our Cmd-based lowering encodes to them. This is the byte-equivalence guarantee for cutover.

    #[test]
    fn vertex_buffer_bytes_match_c_shim() {
        let id = 205u32;
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let got = encode_stream(&vertex_buffer_cmds(id, &data));

        // gl_shim.c: iu8(1); iu32(id); iu64(size); iu32(VERTEX=1); istr("");
        //            iu8(3); iu32(id); iu64(0); ibytes(data);
        let mut e = Encoder::new();
        e.u8(1);
        e.u32(id);
        e.u64(data.len() as u64);
        e.u32(1);
        e.str("");
        e.u8(3);
        e.u32(id);
        e.u64(0);
        e.bytes(&data);
        assert_eq!(got, e.into_vec(), "vertex buffer IR must be byte-identical to gl_shim.c");
    }

    #[test]
    fn index_buffer_usage_is_two() {
        let bytes = encode_stream(&index_buffer_cmds(12, &[0u8; 4]));
        // Byte 0 = CREATE_BUFFER tag, then id(4), size(8), usage(4). usage at offset 1+4+8 = 13.
        let usage = u32::from_le_bytes(bytes[13..17].try_into().unwrap());
        assert_eq!(usage, 2, "index buffer usage must be INDEX=2");
    }

    #[test]
    fn texture_cmds_match_c_shim_bytes() {
        let mut t = Texture {
            used: true,
            w: 4,
            h: 2,
            minf: GL_LINEAR,
            magf: GL_NEAREST,
            ws: GL_CLAMP_TO_EDGE,
            wt: GL_MIRRORED_REPEAT,
            ..Default::default()
        };
        t.data = vec![7u8; (t.w * t.h * 4) as usize];
        let gl_tex = 3u32;

        let cmds = [
            create_texture_cmd(gl_tex, &t),
            create_sampler_cmd(gl_tex, &t),
            texture_staging_cmds(gl_tex, &t)[0].clone(),
            texture_staging_cmds(gl_tex, &t)[1].clone(),
        ];
        let got = encode_stream(&cmds);

        // gl_shim.c hand-emission (lines 2204-2220):
        let tid = 500 + gl_tex;
        let sid = 600 + gl_tex;
        let stg = 700 + gl_tex;
        let mut e = Encoder::new();
        // CreateTexture
        e.u8(4);
        e.u32(tid);
        e.u32(t.w as u32);
        e.u32(t.h as u32);
        e.u32(1);
        e.u32(1);
        e.u32(1);
        e.u32(2); // dim D2
        e.u32(1); // fmt Rgba8Unorm
        e.u32(1 | 4 | 16); // SAMPLED|RENDER_TARGET|COPY_DST
        e.str("");
        // CreateSampler: minf(LINEAR→1), magf(NEAREST→0), mip 0, au(CLAMP→0), av(MIRROR→2), aw 0
        e.u8(6);
        e.u32(sid);
        e.u32(1);
        e.u32(0);
        e.u32(0);
        e.u32(0);
        e.u32(2);
        e.u32(0);
        // staging CreateBuffer(stg,{size,COPY_SRC=16,""}) + WriteBuffer
        e.u8(1);
        e.u32(stg);
        e.u64(t.data.len() as u64);
        e.u32(16);
        e.str("");
        e.u8(3);
        e.u32(stg);
        e.u64(0);
        e.bytes(&t.data);
        assert_eq!(got, e.into_vec(), "texture IR must be byte-identical to gl_shim.c");
    }

    #[test]
    fn lowered_resources_round_trip_through_host_decoder() {
        // The anti-drift gate, extended to the new resource commands: encode via the shim's lowering,
        // decode with the host's own Cmd::decode, assert equality.
        let mut t = Texture { used: true, w: 2, h: 2, minf: GL_NEAREST, magf: GL_LINEAR, ws: GL_REPEAT, wt: GL_REPEAT, ..Default::default() };
        t.data = vec![0xAB; 16];
        let cmds = vec![
            vertex_buffer_cmds(205, &[9u8; 12])[0].clone(),
            vertex_buffer_cmds(205, &[9u8; 12])[1].clone(),
            create_texture_cmd(1, &t),
            create_sampler_cmd(1, &t),
        ];
        let bytes = encode_stream(&cmds);
        let mut d = Decoder::new(&bytes);
        let mut decoded = Vec::new();
        while !d.is_empty() {
            decoded.push(Cmd::decode(&mut d).unwrap());
        }
        assert_eq!(decoded, cmds, "host decoder must reproduce the shim-encoded resource commands");
    }
}
