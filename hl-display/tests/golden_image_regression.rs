//! Deterministic golden-image regression harness for the hl-display Metal renderer.
//!
//! Motivation: rendering correctness (orientation/flip, blend, FBO compositing, glyph sampling) used to
//! be verified by driving a LIVE Chrome through the container — flaky, slow, and non-reproducible. This
//! harness replays *captured* hl-gpu IR streams (exactly the bytes the GL shim forwards for one frame,
//! the same input `hl-display selftest-shim-ir` consumes) through the REAL [`MetalBackend`], reads the
//! composited surface back to RGBA, and pixel-diffs it against a stored golden PNG. Every rendering
//! change is then checked deterministically, with no Chrome and no window server.
//!
//! Layout (all under `<repo>/target-chrome-codex/`):
//!   *.ir                  captured / synthesized IR streams (one frame each)
//!   golden/<name>.png     the reference image (committed)
//!   rendered/<name>.png   this run's output (always written, git-ignored)
//!   diff/<name>.png       per-pixel diff, written on failure (git-ignored)
//!   oracle/<name>.png     optional external oracle (e.g. Weston screenshot) — compared when present
//!
//! Modes (env vars):
//!   HL_UPDATE_GOLDENS=1   (re)write golden/<name>.png from this run instead of asserting — seeds goldens
//!   HL_GOLDEN_TOL=<u8>    per-channel abs tolerance (default 4)
//!   HL_GOLDEN_MAXPCT=<f>  max fraction of pixels allowed to exceed tol (default 0.0 = strict)
//!   HL_ORACLE_STRICT=1    fail the suite if a present oracle image mismatches (default: informational)
//!
//! The IR builders and the PNG codec are platform-agnostic (pure Rust); only the replay+readback step is
//! macOS/Metal. `seed_captures` regenerates the `.ir` files from source on any host, so a reviewer can
//! see exactly what each capture draws. Run via `run_golden.sh` (which uses the `mac` runner for Metal).

// ------------------------------------------------------------------------------------------------
// PNG decode (self-contained). The workspace has a dependency-free PNG *encoder*
// (`hl_ws_term::png::encode_rgba`, stored zlib + filter 0) but no decoder, and this host is offline
// so we cannot pull `png`/`image`. This module decodes 8-bit RGB/RGBA PNGs — the stored-block path used
// by our own goldens *and* the compressed path an external oracle screenshot would use.
// ------------------------------------------------------------------------------------------------
mod png_decode {
    struct BitReader<'a> {
        data: &'a [u8],
        pos: usize,
        bitbuf: u32,
        bitcnt: u32,
    }
    impl<'a> BitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self { data, pos: 0, bitbuf: 0, bitcnt: 0 }
        }
        fn bit(&mut self) -> Result<u32, String> {
            if self.bitcnt == 0 {
                if self.pos >= self.data.len() {
                    return Err("deflate: out of input".into());
                }
                self.bitbuf = self.data[self.pos] as u32;
                self.pos += 1;
                self.bitcnt = 8;
            }
            let b = self.bitbuf & 1;
            self.bitbuf >>= 1;
            self.bitcnt -= 1;
            Ok(b)
        }
        fn bits(&mut self, n: u32) -> Result<u32, String> {
            let mut v = 0u32;
            for i in 0..n {
                v |= self.bit()? << i;
            }
            Ok(v)
        }
        fn align(&mut self) {
            self.bitcnt = 0;
        }
    }

    /// Canonical Huffman table (RFC 1951 / puff.c representation).
    struct Huff {
        counts: [u16; 16],
        symbols: Vec<u16>,
    }
    impl Huff {
        fn build(lengths: &[u16]) -> Huff {
            let mut counts = [0u16; 16];
            for &l in lengths {
                counts[l as usize] += 1;
            }
            counts[0] = 0;
            let mut offs = [0u16; 16];
            for len in 1..16 {
                offs[len] = offs[len - 1] + counts[len - 1];
            }
            let mut symbols = vec![0u16; lengths.len()];
            for (sym, &l) in lengths.iter().enumerate() {
                if l != 0 {
                    symbols[offs[l as usize] as usize] = sym as u16;
                    offs[l as usize] += 1;
                }
            }
            Huff { counts, symbols }
        }
        fn decode(&self, r: &mut BitReader) -> Result<u16, String> {
            let mut code = 0i32;
            let mut first = 0i32;
            let mut index = 0i32;
            for len in 1..16 {
                code |= r.bit()? as i32;
                let count = self.counts[len] as i32;
                if code - count < first {
                    return Ok(self.symbols[(index + (code - first)) as usize]);
                }
                index += count;
                first += count;
                first <<= 1;
                code <<= 1;
            }
            Err("deflate: bad code".into())
        }
    }

    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u16; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u16; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut r = BitReader::new(data);
        let mut out = Vec::new();
        loop {
            let bfinal = r.bit()?;
            let btype = r.bits(2)?;
            match btype {
                0 => {
                    r.align();
                    if r.pos + 4 > r.data.len() {
                        return Err("deflate: stored header past end".into());
                    }
                    let len = r.data[r.pos] as usize | ((r.data[r.pos + 1] as usize) << 8);
                    r.pos += 4; // len + nlen
                    if r.pos + len > r.data.len() {
                        return Err("deflate: stored block past end".into());
                    }
                    out.extend_from_slice(&r.data[r.pos..r.pos + len]);
                    r.pos += len;
                }
                1 | 2 => {
                    let (lit, dist) = if btype == 1 {
                        let mut litlen = [0u16; 288];
                        for (i, l) in litlen.iter_mut().enumerate() {
                            *l = if i < 144 {
                                8
                            } else if i < 256 {
                                9
                            } else if i < 280 {
                                7
                            } else {
                                8
                            };
                        }
                        let distlen = [5u16; 30];
                        (Huff::build(&litlen), Huff::build(&distlen))
                    } else {
                        read_dynamic_tables(&mut r)?
                    };
                    loop {
                        let sym = lit.decode(&mut r)?;
                        if sym == 256 {
                            break;
                        } else if sym < 256 {
                            out.push(sym as u8);
                        } else {
                            let si = (sym - 257) as usize;
                            if si >= LEN_BASE.len() {
                                return Err("deflate: bad length symbol".into());
                            }
                            let length =
                                LEN_BASE[si] as usize + r.bits(LEN_EXTRA[si] as u32)? as usize;
                            let dsym = dist.decode(&mut r)? as usize;
                            if dsym >= DIST_BASE.len() {
                                return Err("deflate: bad dist symbol".into());
                            }
                            let distance =
                                DIST_BASE[dsym] as usize + r.bits(DIST_EXTRA[dsym] as u32)? as usize;
                            if distance > out.len() {
                                return Err("deflate: distance too far back".into());
                            }
                            let start = out.len() - distance;
                            for k in 0..length {
                                out.push(out[start + k]);
                            }
                        }
                    }
                }
                _ => return Err("deflate: reserved btype".into()),
            }
            if bfinal == 1 {
                break;
            }
        }
        Ok(out)
    }

    fn read_dynamic_tables(r: &mut BitReader) -> Result<(Huff, Huff), String> {
        const ORDER: [usize; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];
        let hlit = r.bits(5)? as usize + 257;
        let hdist = r.bits(5)? as usize + 1;
        let hclen = r.bits(4)? as usize + 4;
        let mut cl_lengths = [0u16; 19];
        for i in 0..hclen {
            cl_lengths[ORDER[i]] = r.bits(3)? as u16;
        }
        let cl = Huff::build(&cl_lengths);
        let mut lengths = Vec::with_capacity(hlit + hdist);
        while lengths.len() < hlit + hdist {
            let sym = cl.decode(r)?;
            match sym {
                0..=15 => lengths.push(sym),
                16 => {
                    let prev = *lengths.last().ok_or("deflate: repeat with no prev")?;
                    for _ in 0..(3 + r.bits(2)?) {
                        lengths.push(prev);
                    }
                }
                17 => {
                    for _ in 0..(3 + r.bits(3)?) {
                        lengths.push(0);
                    }
                }
                18 => {
                    for _ in 0..(11 + r.bits(7)?) {
                        lengths.push(0);
                    }
                }
                _ => return Err("deflate: bad code-length symbol".into()),
            }
        }
        if lengths.len() != hlit + hdist {
            return Err("deflate: code length overrun".into());
        }
        let lit = Huff::build(&lengths[..hlit]);
        let dist = Huff::build(&lengths[hlit..]);
        Ok((lit, dist))
    }

    fn paeth(a: i32, b: i32, c: i32) -> i32 {
        let p = a + b - c;
        let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
        if pa <= pb && pa <= pc {
            a
        } else if pb <= pc {
            b
        } else {
            c
        }
    }

    /// Decode an 8-bit RGB or RGBA PNG into (width, height, RGBA8 top-to-bottom).
    pub fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
        const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        if bytes.len() < 8 || bytes[..8] != SIG {
            return Err("not a PNG".into());
        }
        let mut pos = 8;
        let (mut w, mut h) = (0u32, 0u32);
        let mut color_type = 0u8;
        let mut bit_depth = 0u8;
        let mut idat = Vec::new();
        while pos + 8 <= bytes.len() {
            let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                as usize;
            let kind = &bytes[pos + 4..pos + 8];
            let data_start = pos + 8;
            if data_start + len + 4 > bytes.len() {
                return Err("PNG chunk past end".into());
            }
            let data = &bytes[data_start..data_start + len];
            match kind {
                b"IHDR" => {
                    w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                    h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                    bit_depth = data[8];
                    color_type = data[9];
                    if data[12] != 0 {
                        return Err("interlaced PNG unsupported".into());
                    }
                }
                b"IDAT" => idat.extend_from_slice(data),
                b"IEND" => break,
                _ => {}
            }
            pos = data_start + len + 4;
        }
        if bit_depth != 8 || (color_type != 2 && color_type != 6) {
            return Err(format!(
                "unsupported PNG (bit_depth={bit_depth}, color_type={color_type}); want 8-bit RGB/RGBA"
            ));
        }
        if idat.len() < 2 {
            return Err("PNG missing IDAT".into());
        }
        let raw = inflate(&idat[2..])?; // skip 2-byte zlib header; ignore trailing adler32
        let channels = if color_type == 6 { 4usize } else { 3usize };
        let stride = w as usize * channels;
        if raw.len() < (stride + 1) * h as usize {
            return Err(format!(
                "PNG data short: got {} expected {}",
                raw.len(),
                (stride + 1) * h as usize
            ));
        }
        let mut prev = vec![0u8; stride];
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        let mut off = 0usize;
        for _ in 0..h {
            let filter = raw[off];
            off += 1;
            let mut row = raw[off..off + stride].to_vec();
            off += stride;
            for i in 0..stride {
                let a = if i >= channels { row[i - channels] as i32 } else { 0 };
                let b = prev[i] as i32;
                let c = if i >= channels { prev[i - channels] as i32 } else { 0 };
                let x = row[i] as i32;
                row[i] = match filter {
                    0 => x,
                    1 => x + a,
                    2 => x + b,
                    3 => x + (a + b) / 2,
                    4 => x + paeth(a, b, c),
                    _ => return Err(format!("bad filter {filter}")),
                } as u8;
            }
            for px in row.chunks_exact(channels) {
                rgba.push(px[0]);
                rgba.push(px[1]);
                rgba.push(px[2]);
                rgba.push(if channels == 4 { px[3] } else { 255 });
            }
            prev = row;
        }
        Ok((w, h, rgba))
    }
}

// ------------------------------------------------------------------------------------------------
// IR capture builders. Each returns a wire IR byte stream (as `hl_gpu::ir::encode_stream` produces and
// `hl_gpu::replay::replay_stream` consumes). Texture id 1 is the injected render target (the harness's
// readback texture); the streams reference it as the final color attachment.
// ------------------------------------------------------------------------------------------------
mod cases {
    use hl_gpu::ir::*;

    /// Pack MSL source into an IR shader payload: word[0] = byte length, remaining words pack the UTF-8
    /// bytes 4-per-word little-endian (matches `msl_from_words` in metal_backend and the GL shim).
    fn pack_msl(src: &str) -> Vec<u32> {
        let mut words = vec![src.len() as u32];
        for chunk in src.as_bytes().chunks(4) {
            let mut w = [0u8; 4];
            w[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(w));
        }
        words
    }

    fn f32s(out: &mut Vec<u8>, vals: &[f32]) {
        for v in vals {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }

    /// Flat vertex-color shader: pixel-space position -> NDC via Skia's `sk_RTAdjust`, passthrough color.
    const FLAT_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct Uniforms { float4 sk_RTAdjust; };
struct VIn { float2 position [[attribute(0)]]; float4 color [[attribute(1)]]; };
struct VOut { float4 position [[position]]; float4 color [[user(v0)]]; };
vertex VOut vmain(VIn in [[stage_in]], constant Uniforms& u [[buffer(1)]]) {
    VOut out;
    float4 dp = float4(in.position, 0.0, 1.0);
    out.position = float4(dp.xy * u.sk_RTAdjust.xz + dp.ww * u.sk_RTAdjust.yw, 0.0, dp.w);
    out.color = in.color;
    return out;
}
fragment float4 fmain(VOut in [[stage_in]]) { return in.color; }
"#;

    /// Textured shader (Chrome's UI atlas path): samples an atlas at ushort texcoords * invAtlasSize,
    /// modulated by the per-vertex tint. Identical to metal_chrome_ir_regression's HL_TEXTURED_MSL.
    const TEX_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct Uniforms { float4 sk_RTAdjust; float2 invAtlasSize; };
struct VIn { float2 position [[attribute(0)]]; float4 color [[attribute(1)]]; ushort2 texcoord [[attribute(2)]]; };
struct VOut { float4 position [[position]]; float4 color [[user(v0)]]; float2 uv [[user(v1)]]; };
vertex VOut vmain(VIn in [[stage_in]], constant Uniforms& u [[buffer(1)]]) {
    VOut out;
    float4 dp = float4(in.position, 0.0, 1.0);
    out.position = float4(dp.xy * u.sk_RTAdjust.xz + dp.ww * u.sk_RTAdjust.yw, 0.0, dp.w);
    out.color = in.color;
    out.uv = float2(in.texcoord.x, in.texcoord.y) * u.invAtlasSize;
    return out;
}
fragment float4 fmain(VOut in [[stage_in]], texture2d<float> atlas [[texture(0)]], sampler s [[sampler(0)]]) {
    return atlas.sample(s, in.uv) * in.color;
}
"#;

    fn rt_adjust(w: u32, h: u32) -> [f32; 4] {
        [2.0 / w as f32, -1.0, 2.0 / h as f32, -1.0]
    }

    // vertex format codes (see metal_backend::metal_vertex_format): comps | (kind<<8) | (norm<<16).
    const F_FLOAT2: u32 = 2; // float2
    const F_FLOAT4: u32 = 4; // float4
    const F_UCHAR4_NORM: u32 = 0x1_0104; // ubyte4-normalized
    const F_USHORT2: u32 = 0x0302; // ushort2

    fn flat_pipeline(id: u32, shader: u32, label: &str) -> Cmd {
        Cmd::CreateRenderPipeline(
            id,
            RenderPipelineDesc {
                vertex: ShaderRef { module: shader, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: shader, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 24,
                    step_mode: 0,
                    attrs: vec![
                        VertexAttr { location: 0, format: F_FLOAT2, offset: 0 },
                        VertexAttr { location: 1, format: F_FLOAT4, offset: 8 },
                    ],
                }],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xf,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: label.into(),
            },
        )
    }

    fn tex_pipeline(id: u32, shader: u32, label: &str) -> Cmd {
        Cmd::CreateRenderPipeline(
            id,
            RenderPipelineDesc {
                vertex: ShaderRef { module: shader, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: shader, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 16,
                    step_mode: 0,
                    attrs: vec![
                        VertexAttr { location: 0, format: F_FLOAT2, offset: 0 },
                        VertexAttr { location: 1, format: F_UCHAR4_NORM, offset: 8 },
                        VertexAttr { location: 2, format: F_USHORT2, offset: 12 },
                    ],
                }],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xf,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: label.into(),
            },
        )
    }

    /// Emit 6 vertices (two triangles) for an axis-aligned pixel-space quad, flat-shaded.
    fn push_flat_quad(out: &mut Vec<u8>, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 4]) {
        let corners = [(x0, y0), (x0, y1), (x1, y0), (x1, y0), (x0, y1), (x1, y1)];
        for (x, y) in corners {
            f32s(out, &[x, y]);
            f32s(out, &color);
        }
    }

    /// One textured full-frame quad (4 verts, stride 16) covering [0,w]x[0,h] with texcoords [0,tw]x[0,th],
    /// plus the indices [0,1,2,2,1,3].
    fn full_frame_tex_quad(w: u32, h: u32, tw: u16, th: u16, tint: [u8; 4]) -> (Vec<u8>, Vec<u8>) {
        let verts: [([f32; 2], [u16; 2]); 4] = [
            ([0.0, 0.0], [0, 0]),
            ([0.0, h as f32], [0, th]),
            ([w as f32, 0.0], [tw, 0]),
            ([w as f32, h as f32], [tw, th]),
        ];
        let mut vb = Vec::new();
        for (p, t) in verts {
            f32s(&mut vb, &p);
            vb.extend_from_slice(&tint);
            vb.extend_from_slice(&t[0].to_le_bytes());
            vb.extend_from_slice(&t[1].to_le_bytes());
        }
        let mut ib = Vec::new();
        for i in [0u16, 1, 2, 2, 1, 3] {
            ib.extend_from_slice(&i.to_le_bytes());
        }
        (vb, ib)
    }

    fn u8_bytes(u: &[u8]) -> Vec<u8> {
        u.to_vec()
    }

    // -------- Case: solid quads (flat pipeline, 4 colored quadrants over a gray clear) --------
    pub fn solid_quads(w: u32, h: u32) -> Vec<u8> {
        let mut verts = Vec::new();
        let (a, b) = (4.0, (w / 2 - 4) as f32);
        let (c, d) = ((w / 2 + 4) as f32, (w - 4) as f32);
        push_flat_quad(&mut verts, a, a, b, b, [1.0, 0.0, 0.0, 1.0]); // TL red
        push_flat_quad(&mut verts, c, a, d, b, [0.0, 1.0, 0.0, 1.0]); // TR green
        push_flat_quad(&mut verts, a, c, b, d, [0.0, 0.0, 1.0, 1.0]); // BL blue
        push_flat_quad(&mut verts, c, c, d, d, [1.0, 1.0, 0.0, 1.0]); // BR yellow
        let vcount = (verts.len() / 24) as u32;

        let mut unif = Vec::new();
        f32s(&mut unif, &rt_adjust(w, h));

        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: verts.len() as u64, usage: buffer_usage::VERTEX, label: "verts".into() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: verts },
            Cmd::CreateBuffer(12, BufferDesc { size: unif.len() as u64, usage: buffer_usage::UNIFORM, label: "rtadjust".into() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: unif.clone() },
            Cmd::CreateShader { id: 20, kind: hl_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl(FLAT_MSL) },
            flat_pipeline(30, 20, "flat"),
            Cmd::CreateBindGroup(40, BindGroupDesc {
                set: 0,
                entries: vec![BindEntry { binding: 1, resource: BindResource::Buffer { id: 12, offset: 0, size: unif.len() as u64 } }],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.1, 0.1, 0.1, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: w as f32, h: h as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w, h },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: vcount, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        encode_stream(&cmds)
    }

    // Build one textured full-frame quad drawing into target 1 from an RGBA atlas of size (aw,ah).
    fn textured_frame(w: u32, h: u32, aw: u32, ah: u32, atlas: Vec<u8>, tint: [u8; 4]) -> Vec<u8> {
        let (vb, ib) = full_frame_tex_quad(w, h, aw as u16, ah as u16, tint);
        let mut unif = Vec::new();
        f32s(&mut unif, &rt_adjust(w, h));
        f32s(&mut unif, &[1.0 / aw as f32, 1.0 / ah as f32, 0.0, 0.0]); // invAtlasSize + pad
        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: vb.len() as u64, usage: buffer_usage::VERTEX, label: "verts".into() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: vb },
            Cmd::CreateBuffer(11, BufferDesc { size: ib.len() as u64, usage: buffer_usage::INDEX, label: "idx".into() }),
            Cmd::WriteBuffer { id: 11, offset: 0, data: ib },
            Cmd::CreateBuffer(12, BufferDesc { size: unif.len() as u64, usage: buffer_usage::UNIFORM, label: "unif".into() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: unif.clone() },
            Cmd::CreateBuffer(13, BufferDesc { size: atlas.len() as u64, usage: buffer_usage::COPY_SRC, label: "atlas-staging".into() }),
            Cmd::WriteBuffer { id: 13, offset: 0, data: atlas },
            Cmd::CreateTexture(2, TextureDesc {
                width: aw, height: ah, depth: 1, mip_levels: 1, sample_count: 1,
                dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::SAMPLED | texture_usage::COPY_DST, label: "atlas".into(),
            }),
            Cmd::CreateSampler(3, SamplerDesc {
                min_filter: Filter::Nearest, mag_filter: Filter::Nearest, mip_filter: Filter::Nearest,
                address_u: AddressMode::ClampToEdge, address_v: AddressMode::ClampToEdge, address_w: AddressMode::ClampToEdge,
            }),
            Cmd::CreateShader { id: 20, kind: hl_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl(TEX_MSL) },
            tex_pipeline(30, 20, "textured"),
            Cmd::CreateBindGroup(40, BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry { binding: 1, resource: BindResource::Buffer { id: 12, offset: 0, size: unif.len() as u64 } },
                    BindEntry { binding: 0, resource: BindResource::Texture { id: 2 } },
                    BindEntry { binding: 0, resource: BindResource::Sampler { id: 3 } },
                ],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture { src: 13, src_offset: 0, bytes_per_row: aw * 4, dst: 2, mip: 0, width: aw, height: ah },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: w as f32, h: h as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w, h },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::SetIndexBuffer { buffer: 11, offset: 0, format: IndexFormat::U16 },
                    Enc::DrawIndexed { index_count: 6, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        encode_stream(&cmds)
    }

    // -------- Case: textured glyph (an 8x8 'F' atlas, asymmetric in both axes) --------
    pub fn textured_glyph(w: u32, h: u32) -> Vec<u8> {
        // 'F' glyph, row 0 = top. 1 = ink.
        #[rustfmt::skip]
        const F: [[u8; 8]; 8] = [
            [0,1,1,1,1,1,0,0],
            [0,1,0,0,0,0,0,0],
            [0,1,0,0,0,0,0,0],
            [0,1,1,1,1,0,0,0],
            [0,1,0,0,0,0,0,0],
            [0,1,0,0,0,0,0,0],
            [0,1,0,0,0,0,0,0],
            [0,0,0,0,0,0,0,0],
        ];
        let mut atlas = Vec::with_capacity(8 * 8 * 4);
        for row in F {
            for cell in row {
                if cell == 1 {
                    atlas.extend_from_slice(&[255, 255, 255, 255]);
                } else {
                    atlas.extend_from_slice(&[20, 20, 40, 255]);
                }
            }
        }
        textured_frame(w, h, 8, 8, atlas, [255, 255, 255, 255])
    }

    // -------- Case: orientation (2x2 four-color atlas — any flip/transpose permutes quadrants) --------
    pub fn orientation(w: u32, h: u32) -> Vec<u8> {
        // This fixture deliberately uses Chrome/GL pixel coordinates: rt_adjust maps y=0 to NDC -1,
        // the bottom of the presented surface. Texture row zero is therefore sampled into the
        // bottom half. The semantic assertion below pins that contract independently of PNG goldens.
        let atlas = u8_bytes(&[
            255, 0, 0, 255, /*row 0 left: red*/ 0, 255, 0, 255, /*row 0 right: green*/
            0, 0, 255, 255, /*row 1 left: blue*/ 255, 255, 255, 255, /*row 1 right: white*/
        ]);
        textured_frame(w, h, 2, 2, atlas, [255, 255, 255, 255])
    }

    // -------- Case: offscreen FBO composite (render solid to tex 2, then sample tex 2 into target 1) --------
    pub fn offscreen_fbo(w: u32, h: u32) -> Vec<u8> {
        // Pass 1: flat solid quads into offscreen texture id 2.
        let mut verts = Vec::new();
        push_flat_quad(&mut verts, 0.0, 0.0, (w / 2) as f32, h as f32, [0.0, 0.8, 0.2, 1.0]); // left green
        push_flat_quad(&mut verts, (w / 2) as f32, 0.0, w as f32, (h / 2) as f32, [0.9, 0.1, 0.1, 1.0]); // TR red
        push_flat_quad(&mut verts, (w / 2) as f32, (h / 2) as f32, w as f32, h as f32, [0.1, 0.2, 0.9, 1.0]); // BR blue
        let vcount = (verts.len() / 24) as u32;
        let mut unif0 = Vec::new();
        f32s(&mut unif0, &rt_adjust(w, h));

        // Pass 2: textured full-frame quad sampling the offscreen texture (tw=w, th=h, invAtlasSize=1/w,1/h).
        let (vb, ib) = full_frame_tex_quad(w, h, w as u16, h as u16, [255, 255, 255, 255]);
        let mut unif1 = Vec::new();
        f32s(&mut unif1, &rt_adjust(w, h));
        f32s(&mut unif1, &[1.0 / w as f32, 1.0 / h as f32, 0.0, 0.0]);

        let cmds = vec![
            // offscreen render target
            Cmd::CreateTexture(2, TextureDesc {
                width: w, height: h, depth: 1, mip_levels: 1, sample_count: 1,
                dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET, label: "fbo".into(),
            }),
            // pass 1 resources
            Cmd::CreateBuffer(10, BufferDesc { size: verts.len() as u64, usage: buffer_usage::VERTEX, label: "fbo-verts".into() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: verts },
            Cmd::CreateBuffer(12, BufferDesc { size: unif0.len() as u64, usage: buffer_usage::UNIFORM, label: "fbo-unif".into() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: unif0.clone() },
            Cmd::CreateShader { id: 20, kind: hl_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl(FLAT_MSL) },
            flat_pipeline(30, 20, "flat"),
            Cmd::CreateBindGroup(40, BindGroupDesc {
                set: 0,
                entries: vec![BindEntry { binding: 1, resource: BindResource::Buffer { id: 12, offset: 0, size: unif0.len() as u64 } }],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 2, load: LoadOp::Clear, clear: [0.05, 0.05, 0.05, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: w as f32, h: h as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w, h },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: vcount, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
            // pass 2 resources
            Cmd::CreateBuffer(14, BufferDesc { size: vb.len() as u64, usage: buffer_usage::VERTEX, label: "comp-verts".into() }),
            Cmd::WriteBuffer { id: 14, offset: 0, data: vb },
            Cmd::CreateBuffer(15, BufferDesc { size: ib.len() as u64, usage: buffer_usage::INDEX, label: "comp-idx".into() }),
            Cmd::WriteBuffer { id: 15, offset: 0, data: ib },
            Cmd::CreateBuffer(16, BufferDesc { size: unif1.len() as u64, usage: buffer_usage::UNIFORM, label: "comp-unif".into() }),
            Cmd::WriteBuffer { id: 16, offset: 0, data: unif1.clone() },
            Cmd::CreateSampler(3, SamplerDesc {
                min_filter: Filter::Nearest, mag_filter: Filter::Nearest, mip_filter: Filter::Nearest,
                address_u: AddressMode::ClampToEdge, address_v: AddressMode::ClampToEdge, address_w: AddressMode::ClampToEdge,
            }),
            Cmd::CreateShader { id: 21, kind: hl_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl(TEX_MSL) },
            tex_pipeline(31, 21, "textured"),
            Cmd::CreateBindGroup(41, BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry { binding: 1, resource: BindResource::Buffer { id: 16, offset: 0, size: unif1.len() as u64 } },
                    BindEntry { binding: 0, resource: BindResource::Texture { id: 2 } },
                    BindEntry { binding: 0, resource: BindResource::Sampler { id: 3 } },
                ],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(31),
                    Enc::SetBindGroup { index: 0, group: 41 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: w as f32, h: h as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w, h },
                    Enc::SetVertexBuffer { slot: 0, buffer: 14, offset: 0 },
                    Enc::SetIndexBuffer { buffer: 15, offset: 0, format: IndexFormat::U16 },
                    Enc::DrawIndexed { index_count: 6, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        encode_stream(&cmds)
    }
}

// ------------------------------------------------------------------------------------------------
// Case registry, shared by seeding (any host) and the Metal golden run (macOS).
// ------------------------------------------------------------------------------------------------
struct Case {
    name: &'static str,
    w: u32,
    h: u32,
    /// Builder for a synthesized capture; `None` = external capture slot (the `.ir` must be provided).
    build: Option<fn(u32, u32) -> Vec<u8>>,
    /// Optional external oracle image under `oracle/` to cross-check against (e.g. a Weston screenshot).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    oracle: Option<&'static str>,
}

fn registry() -> Vec<Case> {
    vec![
        Case { name: "chrome-solid-quads", w: 64, h: 64, build: Some(cases::solid_quads), oracle: None },
        Case { name: "chrome-textured-glyph", w: 64, h: 64, build: Some(cases::textured_glyph), oracle: None },
        Case { name: "chrome-offscreen-fbo", w: 64, h: 64, build: Some(cases::offscreen_fbo), oracle: None },
        Case { name: "chrome-orientation", w: 64, h: 64, build: Some(cases::orientation), oracle: None },
        // External-capture slot: drop a real GL-shim capture here (HL_IR_DUMP) as
        // target-chrome-codex/chrome-stream-ir-000.ir + a golden, and (when it lands) the Weston oracle
        // screenshot at oracle/chrome-weston.png. Skipped cleanly until the .ir is present.
        Case { name: "chrome-stream-ir-000", w: 1280, h: 720, build: None, oracle: Some("chrome-weston.png") },
    ]
}

fn codex_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("hl-display has a parent dir")
        .join("target-chrome-codex")
}

fn golden_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn assert_orientation_contract(rgba: &[u8], w: u32, h: u32) {
    let pixel = |x: u32, y: u32| {
        let offset = ((y * w + x) * 4) as usize;
        &rgba[offset..offset + 4]
    };
    assert_eq!(pixel(w / 4, h / 4), [0, 0, 255, 255], "surface top-left must sample GL row 1");
    assert_eq!(pixel(3 * w / 4, h / 4), [255, 255, 255, 255], "surface top-right must sample GL row 1");
    assert_eq!(pixel(w / 4, 3 * h / 4), [255, 0, 0, 255], "surface bottom-left must sample GL row 0");
    assert_eq!(pixel(3 * w / 4, 3 * h / 4), [0, 255, 0, 255], "surface bottom-right must sample GL row 0");
}

/// Regenerate the synthesized `.ir` capture files from the builders above. Runs on any host (no Metal),
/// so a reviewer can reproduce/inspect exactly what each golden draws:
///   cargo test -p hl-display --test golden_image_regression -- seed_captures --nocapture
#[test]
fn seed_captures() {
    let root = codex_root();
    std::fs::create_dir_all(&root).unwrap();
    for c in registry() {
        if let Some(build) = c.build {
            let bytes = build(c.w, c.h);
            let p = root.join(format!("{}.ir", c.name));
            std::fs::write(&p, &bytes).unwrap();
            println!("seeded {} ({} IR bytes)", p.display(), bytes.len());
        }
    }
}

/// Round-trip the pure-Rust PNG encoder through our decoder so the golden-compare path is trusted even
/// on non-Metal hosts (guards the stored-block + filter-0 decode our own goldens use).
#[test]
fn png_codec_roundtrip() {
    let (w, h) = (5u32, 3u32);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[(x * 40) as u8, (y * 70) as u8, (x + y) as u8, 255]);
        }
    }
    let png = hl_ws_term::png::encode_rgba(w, h, &rgba);
    let (dw, dh, drgba) = png_decode::decode(&png).expect("decode our own PNG");
    assert_eq!((dw, dh), (w, h));
    assert_eq!(drgba, rgba);
}

#[cfg(target_os = "macos")]
mod metal_suite {
    use super::{assert_orientation_contract, codex_root, golden_root, png_decode, registry};
    use hl_display::metal::MetalCtx;
    use hl_display::metal_backend::MetalBackend;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{
        MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
    };

    fn new_rgba_target(ctx: &MetalCtx, w: u32, h: u32) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                w as usize,
                h as usize,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Shared);
        ctx.device.newTextureWithDescriptor(&desc).expect("newTexture")
    }

    fn read_rgba(tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        let region = objc2_metal::MTLRegion {
            origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
            size: objc2_metal::MTLSize { width: w as usize, height: h as usize, depth: 1 },
        };
        unsafe {
            tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(out.as_mut_ptr() as *mut _).unwrap(),
                (w * 4) as usize,
                region,
                0,
            );
        }
        out
    }

    fn render_ir(ir: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
        let ctx = MetalCtx::new()?;
        let target = new_rgba_target(&ctx, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, target.clone());
        if let Err(e) = hl_gpu::replay::replay_stream(&mut be, ir) {
            panic!("replay error: {e}");
        }
        Some(read_rgba(&target, w, h))
    }

    struct DiffStats {
        max_channel_diff: u32,
        differing: u64,
        total: u64,
    }
    impl DiffStats {
        fn pct(&self) -> f64 {
            if self.total == 0 { 0.0 } else { self.differing as f64 / self.total as f64 }
        }
    }

    /// Per-pixel diff: `differing` counts pixels whose max channel abs-diff exceeds `tol`. Also returns a
    /// visualization: over-tolerance pixels are pure red; within-tolerance ones show the amplified diff.
    fn diff(a: &[u8], b: &[u8], tol: u8) -> (DiffStats, Vec<u8>) {
        let total = (a.len() / 4) as u64;
        let mut max_channel_diff = 0u32;
        let mut differing = 0u64;
        let mut img = vec![0u8; a.len()];
        for i in 0..total as usize {
            let mut pixel_max = 0u32;
            for c in 0..4 {
                let d = (a[i * 4 + c] as i32 - b[i * 4 + c] as i32).unsigned_abs();
                pixel_max = pixel_max.max(d);
            }
            max_channel_diff = max_channel_diff.max(pixel_max);
            if pixel_max > tol as u32 {
                differing += 1;
                img[i * 4..i * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            } else {
                let v = (pixel_max * 8).min(255) as u8;
                img[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        (DiffStats { max_channel_diff, differing, total }, img)
    }

    fn env_u8(key: &str, default: u8) -> u8 {
        std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }
    fn env_f64(key: &str, default: f64) -> f64 {
        std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    #[test]
    fn golden_suite() {
        // Probe once; if there is no Metal device (headless CI without a GPU) skip rather than fail.
        if MetalCtx::new().is_none() {
            eprintln!("SKIP golden_suite: no Metal device");
            return;
        }
        let root = codex_root();
        let golden_root = golden_root();
        let golden_dir = golden_root.join("images");
        let rendered_dir = root.join("rendered");
        let diff_dir = root.join("diff");
        let oracle_dir = golden_root.join("oracle");
        for d in [&golden_dir, &rendered_dir, &diff_dir, &oracle_dir] {
            std::fs::create_dir_all(d).unwrap();
        }

        let update = std::env::var("HL_UPDATE_GOLDENS").is_ok();
        let tol = env_u8("HL_GOLDEN_TOL", 4);
        let max_pct = env_f64("HL_GOLDEN_MAXPCT", 0.0);
        let oracle_strict = std::env::var("HL_ORACLE_STRICT").is_ok();

        let mut failures = Vec::new();
        println!(
            "\n=== golden_suite (tol={tol}, max_pct={max_pct}, {}) ===",
            if update { "UPDATE" } else { "CHECK" }
        );

        for c in registry() {
            let ir_path = root.join(format!("{}.ir", c.name));
            // Synthesized cases: always regenerate the capture so it tracks the builder source.
            if let Some(build) = c.build {
                std::fs::write(&ir_path, build(c.w, c.h)).unwrap();
            }
            if !ir_path.exists() {
                println!("  SKIP  {:<24} (no capture at {})", c.name, ir_path.display());
                continue;
            }
            let ir = std::fs::read(&ir_path).unwrap();
            let rgba = match render_ir(&ir, c.w, c.h) {
                Some(r) => r,
                None => {
                    println!("  SKIP  {:<24} (no Metal device)", c.name);
                    continue;
                }
            };
            if c.name == "chrome-orientation" {
                assert_orientation_contract(&rgba, c.w, c.h);
            }
            let rendered_png = hl_ws_term::png::encode_rgba(c.w, c.h, &rgba);
            std::fs::write(rendered_dir.join(format!("{}.png", c.name)), &rendered_png).unwrap();

            let golden_path = golden_dir.join(format!("{}.png", c.name));
            if update {
                std::fs::write(&golden_path, &rendered_png).unwrap();
                println!("  UPDATE {:<24} -> {}", c.name, golden_path.display());
            } else if !golden_path.exists() {
                println!("  FAIL  {:<24} (no golden; run with HL_UPDATE_GOLDENS=1)", c.name);
                failures.push(c.name);
            } else {
                match png_decode::decode(&std::fs::read(&golden_path).unwrap()) {
                    Ok((gw, gh, golden)) if (gw, gh) == (c.w, c.h) => {
                        let (stats, diff_img) = diff(&rgba, &golden, tol);
                        let pass = stats.pct() <= max_pct;
                        println!(
                            "  {}  {:<24} maxΔ={} differing={}/{} ({:.4}%)",
                            if pass { "PASS" } else { "FAIL" },
                            c.name,
                            stats.max_channel_diff,
                            stats.differing,
                            stats.total,
                            stats.pct() * 100.0
                        );
                        if !pass {
                            let dp = diff_dir.join(format!("{}.png", c.name));
                            std::fs::write(&dp, hl_ws_term::png::encode_rgba(c.w, c.h, &diff_img)).unwrap();
                            println!("        wrote diff {}", dp.display());
                            failures.push(c.name);
                        }
                    }
                    Ok((gw, gh, _)) => {
                        println!("  FAIL  {:<24} golden is {gw}x{gh}, rendered {}x{}", c.name, c.w, c.h);
                        failures.push(c.name);
                    }
                    Err(e) => {
                        println!("  FAIL  {:<24} golden decode error: {e}", c.name);
                        failures.push(c.name);
                    }
                }
            }

            // Optional external oracle (informational unless HL_ORACLE_STRICT).
            if let Some(oracle_name) = c.oracle {
                let op = oracle_dir.join(oracle_name);
                if op.exists() {
                    match png_decode::decode(&std::fs::read(&op).unwrap()) {
                        Ok((ow, oh, oracle)) if (ow, oh) == (c.w, c.h) => {
                            let (stats, _) = diff(&rgba, &oracle, tol);
                            let ok = stats.pct() <= max_pct;
                            println!(
                                "  ORACLE {} {:<22} maxΔ={} differing {:.4}%",
                                if ok { "match" } else { "DIFF " },
                                oracle_name,
                                stats.max_channel_diff,
                                stats.pct() * 100.0
                            );
                            if !ok && oracle_strict {
                                failures.push(c.name);
                            }
                        }
                        Ok((ow, oh, _)) => println!(
                            "  ORACLE skip  {oracle_name} is {ow}x{oh}, rendered {}x{}",
                            c.w, c.h
                        ),
                        Err(e) => println!("  ORACLE skip  {oracle_name} decode: {e}"),
                    }
                }
            }
        }

        if !update && !failures.is_empty() {
            panic!("golden regressions: {failures:?} (see target-chrome-codex/diff/)");
        }
    }
}

/// wgpu-backend golden parity: replay the SAME captured IR through `hl_gpu_wgpu::WgpuBackend` and diff the
/// readback against the Metal-produced golden PNGs (`golden/<name>.png`). This is the metal-vs-wgpu pixel
/// comparison the wgpu present-path wiring is validated by — run after the Metal `golden_suite` has seeded
/// goldens (HL_UPDATE_GOLDENS=1). Reuses the same `registry()`/`cases` builders, `png_decode`, and diff.
#[cfg(target_os = "macos")]
mod wgpu_suite {
    use super::{assert_orientation_contract, codex_root, golden_root, png_decode, registry};
    use hl_gpu::ir::TextureFormat;
    use hl_gpu_wgpu::WgpuBackend;

    fn render_ir(ir: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
        let mut be = WgpuBackend::new().ok()?;
        be.create_render_target(1, w, h, TextureFormat::Rgba8Unorm);
        if let Err(e) = hl_gpu::replay::replay_stream(&mut be, ir) {
            panic!("wgpu replay error: {e}");
        }
        be.read_target(1).ok()
    }

    fn diff(a: &[u8], b: &[u8], tol: u8) -> (u32, u64, u64, Vec<u8>) {
        let total = (a.len() / 4) as u64;
        let (mut maxd, mut differing) = (0u32, 0u64);
        let mut img = vec![0u8; a.len()];
        for i in 0..total as usize {
            let mut pm = 0u32;
            for c in 0..4 {
                pm = pm.max((a[i * 4 + c] as i32 - b[i * 4 + c] as i32).unsigned_abs());
            }
            maxd = maxd.max(pm);
            if pm > tol as u32 {
                differing += 1;
                img[i * 4..i * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            } else {
                let v = (pm * 8).min(255) as u8;
                img[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        (maxd, differing, total, img)
    }

    fn env_u8(key: &str, default: u8) -> u8 {
        std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }
    fn env_f64(key: &str, default: f64) -> f64 {
        std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    #[test]
    fn wgpu_golden_suite() {
        if WgpuBackend::new().is_err() {
            eprintln!("SKIP wgpu_golden_suite: no wgpu Metal device");
            return;
        }
        let root = codex_root();
        let golden_dir = golden_root().join("images");
        let rendered_dir = root.join("rendered-wgpu");
        let diff_dir = root.join("diff-wgpu");
        for d in [&rendered_dir, &diff_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        let tol = env_u8("HL_GOLDEN_TOL", 4);
        let max_pct = env_f64("HL_GOLDEN_MAXPCT", 0.0);
        // The wgpu path is a distinct rasterizer; sub-pixel edges of rotated/offscreen quads can differ
        // from Metal by a texel. Allow a looser default just for wgpu-vs-Metal parity unless overridden.
        let wgpu_max_pct = env_f64("HL_WGPU_MAXPCT", max_pct.max(0.02));

        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        println!("\n=== wgpu_golden_suite (tol={tol}, wgpu_max_pct={wgpu_max_pct}) ===");
        for c in registry() {
            let Some(build) = c.build else { continue };
            let ir = build(c.w, c.h);
            let rgba = match render_ir(&ir, c.w, c.h) {
                Some(r) => r,
                None => {
                    println!("  SKIP  {:<24} (no wgpu device)", c.name);
                    continue;
                }
            };
            if c.name == "chrome-orientation" {
                assert_orientation_contract(&rgba, c.w, c.h);
            }
            std::fs::write(
                rendered_dir.join(format!("{}.png", c.name)),
                hl_ws_term::png::encode_rgba(c.w, c.h, &rgba),
            )
            .unwrap();

            let golden_path = golden_dir.join(format!("{}.png", c.name));
            if !golden_path.exists() {
                println!("  SKIP  {:<24} (no Metal golden; run golden_suite --update first)", c.name);
                skipped.push(c.name);
                continue;
            }
            match png_decode::decode(&std::fs::read(&golden_path).unwrap()) {
                Ok((gw, gh, golden)) if (gw, gh) == (c.w, c.h) => {
                    let (maxd, differing, total, img) = diff(&rgba, &golden, tol);
                    let pct = if total == 0 { 0.0 } else { differing as f64 / total as f64 };
                    let pass = pct <= wgpu_max_pct;
                    println!(
                        "  {}  {:<24} maxΔ={maxd} differing={differing}/{total} ({:.4}%)",
                        if pass { "PASS" } else { "FAIL" },
                        c.name,
                        pct * 100.0
                    );
                    if !pass {
                        std::fs::write(
                            diff_dir.join(format!("{}.png", c.name)),
                            hl_ws_term::png::encode_rgba(c.w, c.h, &img),
                        )
                        .unwrap();
                        failures.push(c.name);
                    }
                }
                Ok((gw, gh, _)) => {
                    println!("  FAIL  {:<24} golden is {gw}x{gh}, rendered {}x{}", c.name, c.w, c.h);
                    failures.push(c.name);
                }
                Err(e) => {
                    println!("  FAIL  {:<24} golden decode error: {e}", c.name);
                    failures.push(c.name);
                }
            }
        }
        if !skipped.is_empty() {
            eprintln!("wgpu_golden_suite: {} case(s) skipped (no golden yet): {skipped:?}", skipped.len());
        }
        assert!(failures.is_empty(), "wgpu-vs-Metal golden diffs: {failures:?} (see target-chrome-codex/diff-wgpu/)");
    }

    /// L3 pipeline cache: the GL shim re-emits `CreateRenderPipeline` under the same id every frame. Prove
    /// that after the first (compiling) call, an identical descriptor — whether re-emitted under the SAME id
    /// (id-hash short-circuit) or a DIFFERENT id (content-cache hit) — compiles NOTHING, while a genuinely
    /// different descriptor still compiles. This is the steady-state win; combined with the golden suite's
    /// maxΔ=0 it establishes both halves of the claim (fewer compiles, unchanged pixels — a cache hit hands
    /// back the very same compiled pipeline object, so output is identical by construction).
    #[test]
    fn wgpu_pipeline_cache_steady_state() {
        use hl_gpu::backend::GpuBackend as _;
        use hl_gpu::id::PipelineId;
        use hl_gpu::ir::{ColorTargetState, RenderPipelineDesc, ShaderRef, Topology};

        let mut be = match WgpuBackend::new() {
            Ok(b) => b,
            Err(_) => {
                eprintln!("SKIP wgpu_pipeline_cache_steady_state: no wgpu Metal device");
                return;
            }
        };
        let desc = |mask: u32| RenderPipelineDesc {
            vertex: ShaderRef { module: 20, entry: "vs".into() }, // module 20 not created ⇒ builtin Flat
            fragment: Some(ShaderRef { module: 20, entry: "fs".into() }),
            vertex_buffers: vec![],
            color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: mask }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            label: "cache-probe".into(),
        };

        be.create_render_pipeline(PipelineId(30), &desc(0xf)).unwrap();
        let (_, cold, _) = be.take_prof();
        assert_eq!(cold, 1, "first pipeline create should compile exactly once");

        // Same id, same descriptor (the every-frame re-emit) → id-hash short-circuit, zero compiles.
        be.create_render_pipeline(PipelineId(30), &desc(0xf)).unwrap();
        assert_eq!(be.take_prof().1, 0, "re-emit of identical pipeline must not recompile");

        // Different id, same content → content-cache hit, zero compiles.
        be.create_render_pipeline(PipelineId(31), &desc(0xf)).unwrap();
        assert_eq!(be.take_prof().1, 0, "identical content under a new id must hit the content cache");

        // Genuinely different descriptor → miss, one compile (proves the key discriminates).
        be.create_render_pipeline(PipelineId(30), &desc(0x7)).unwrap();
        assert_eq!(be.take_prof().1, 1, "a changed descriptor must recompile");

        println!("wgpu_pipeline_cache_steady_state: OK (steady-state pipeline compiles = 0)");
    }
}
