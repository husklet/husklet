//! GL texture objects + the texture table: `glGenTextures`/`glTexImage2D`/`glTexParameteri` tracking.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Texture`, the `MAXTEX` array) + the resource lowering in
//! `hl-shim-gl/src/lower.rs`. A GL texture keeps a CPU shadow plane in the texel format its `ir_format`
//! names (converted from the app's upload format) + filter/wrap state; the frame builder
//! ([`crate::service::frame`]) lowers a sampled texture
//! to a `CreateTexture` + `CreateSampler` + a staging `CreateBuffer`/`WriteBuffer` + a
//! `CopyBufferToTexture`, exactly as `gl_shim.c` does at swap.

use super::glconst::*;
use hl_gpu::protocol::model::enums::{AddressMode, Filter, TextureFormat};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};

static NEXT_SHARED_PIXELS_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CubeImageDesc {
    w: i32,
    h: i32,
    internal_format: u32,
}

/// Pixel storage shared by every GL texture sibling created from one linear EGLImage allocation.
///
/// The GL texture object remains a distinct view with its own sampler state and object generation. Pixel
/// content does not: a producer render, CPU upload, or copy updates the storage version observed by every
/// sibling, including texture snapshots retained after the GL name or EGLImage handle is deleted.
#[derive(Debug)]
pub struct SharedPixels {
    id: u64,
    state: RwLock<PixelState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PixelSnapshot {
    pub version: u64,
    pub data: Arc<Vec<u8>>,
}

#[derive(Debug)]
struct PixelState {
    version: u64,
    data: Arc<Vec<u8>>,
}

/// Bytes per texel of the plane a texture materializes, which is what its zeroed storage must be sized
/// by. This was hardcoded to four, so a plane whose format is wider than RGBA8 was allocated at half or a
/// quarter of its own size: `glTexStorage2D(GL_RGBA16F, 8, 8)` declared an `Rgba16Float` plane and gave it
/// 256 bytes where the format needs 512. Nothing narrowed the format to match, so the plane and its
/// declared format disagreed about how big a texel is, and every consumer that trusts the format reads
/// past the end or short.
///
/// Four is both the fallback for a format with no host texel size (the depth formats) and a FLOOR. The
/// floor is not tidiness: the CPU shadow is an RGBA8 image for every narrow format — `glTexSubImage2D`,
/// the readback path and the render-target shadow all address it at four bytes per texel — so sizing an
/// `R8Unorm` plane at its own one byte per texel makes those writers run off the end. Only the formats
/// WIDER than RGBA8 were under-allocated, and only they change here.
fn plane_bytes_per_texel(format: TextureFormat) -> usize {
    format.bytes_per_texel().unwrap_or(4).max(4)
}

impl SharedPixels {
    pub fn new(data: Arc<Vec<u8>>) -> Self {
        Self {
            id: NEXT_SHARED_PIXELS_ID.fetch_add(1, Ordering::Relaxed),
            state: RwLock::new(PixelState { version: 1, data }),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn version(&self) -> u64 {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .version
    }

    pub fn load(&self) -> Arc<Vec<u8>> {
        self.snapshot().data
    }

    pub fn snapshot(&self) -> PixelSnapshot {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        PixelSnapshot {
            version: state.version,
            data: state.data.clone(),
        }
    }

    pub fn store(&self, data: Arc<Vec<u8>>) -> u64 {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.version = state.version.saturating_add(1);
        state.data = data;
        state.version
    }
}

impl PartialEq for SharedPixels {
    fn eq(&self, other: &Self) -> bool {
        self.version() == other.version() && self.load() == other.load()
    }
}

/// One mip level above the base (see [`GlTexture::mips`]): its extent and pixels, in the texel format of
/// the texture's plane.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct MipLevel {
    pub w: i32,
    pub h: i32,
    pub depth: i32,
    pub data: Arc<Vec<u8>>,
    pub(crate) layers: Vec<Arc<Vec<u8>>>,
}

impl MipLevel {
    pub fn layer(&self, layer: usize) -> Option<&[u8]> {
        if layer == 0 {
            Some(self.data.as_slice())
        } else {
            self.layers.get(layer - 1).map(|pixels| pixels.as_slice())
        }
    }
}

/// One live GL texture object: its CPU shadow plane + the min/mag filter + S/T wrap GL enums.
#[derive(Clone, PartialEq, Debug)]
pub struct GlTexture {
    pub w: i32,
    pub h: i32,
    pub depth: i32,
    /// The CPU shadow plane: `w * h * `[`GlTexture::bytes_per_texel`] bytes, in the texel format
    /// [`GlTexture::ir_format`] names.
    ///
    /// This was documented, and addressed, as RGBA8 (`w*h*4`) unconditionally. The size half stopped being
    /// true once immutable storage started materializing real half-float and float planes, and the two
    /// halves disagreeing is a silent corruption rather than an error: a full-extent `glTexSubImage2D` into
    /// a 4x4 `Rgba16Float` plane wrote 64 of its 128 bytes, at a stride of four, and returned success.
    /// Every reader and writer of this field asks the texture for its texel size instead.
    pub data: Arc<Vec<u8>>,
    /// Base-level cube faces in GL order (+X, -X, +Y, -Y, +Z, -Z). `data` remains the canonical 2D
    /// plane for existing image/FBO paths; cube sampling lowers these six distinct planes as layers.
    pub(crate) cube_faces: [Option<Arc<Vec<u8>>>; 6],
    /// Per-face declarations retain the GL spelling as well as the extent. The neutral plane cannot
    /// distinguish RGB from RGBA or LUMINANCE from LUMINANCE_ALPHA, but cube completeness must.
    pub(crate) cube_face_descs: [Option<CubeImageDesc>; 6],
    /// Per-level, per-face declarations above the base. `mips` owns the bytes used by lowering; this
    /// parallel metadata owns the GL completeness rules that cannot be recovered from those bytes.
    pub(crate) cube_mip_descs: Vec<[Option<CubeImageDesc>; 6]>,
    pub(crate) layers: Vec<Arc<Vec<u8>>>,
    pub min_filter: u32,
    pub mag_filter: u32,
    pub wrap_s: u32,
    pub wrap_t: u32,
    /// Component mapping applied to a sampled texel (`GL_TEXTURE_SWIZZLE_*`).
    pub swizzle: [u32; 4],
    pub gen: u64,
    /// Immutable-format storage (`glTexStorage2D`/`3D`): once set, `glTexImage2D` on this texture is a
    /// `GL_INVALID_OPERATION` and only `glTexSubImage*` may update its pixels.
    pub immutable: bool,
    /// The mip-level count declared by `glTexStorage*` (`1` for a mutable texture — the base level only).
    pub levels: i32,
    /// `GL_TEXTURE_BASE_LEVEL` / `GL_TEXTURE_MAX_LEVEL` — the level range the app declared usable.
    pub base_level: i32,
    pub max_level: i32,
    /// Mip levels ABOVE the base: `mips[i]` is level `i + 1`. EMPTY for the overwhelmingly common
    /// single-level texture, whose base image stays in `data`/`w`/`h` exactly as before — so every reader
    /// of those fields (FBO attachments, readback, shared EGLImage pixels) is untouched.
    ///
    /// `glTexImage2D` used to IGNORE its `level` argument, so each level of a mip chain redefined the base
    /// image and the last, 1x1 upload won: a mipmapped texture collapsed to one texel and every draw
    /// sampling it read a flat colour.
    pub mips: Vec<MipLevel>,
    /// The SIZED GL internal format the application declared (`0` when it gave an unsized one, which is
    /// RGBA8 here). The neutral `ir_format` cannot stand in for it: `GL_SRGB8`, `GL_RGB9_E5`,
    /// `GL_RGB8_SNORM` and `GL_RGB8UI` all materialize as an RGBA8 plane, yet none of them is
    /// colour-renderable, and telling them apart is what a framebuffer completeness check needs.
    pub internal_format: u32,
    /// The neutral-IR texel format this texture lowers to (a `CreateTexture` `format`, and — when the
    /// texture is a framebuffer color attachment — the render-target + surface format). Chosen from the
    /// `glTexImage2D` internal format; defaults to `Rgba8Unorm` (the RGBA8 upload the model materializes).
    pub ir_format: TextureFormat,
    /// Whether this texture's `data` plane holds REAL uploaded pixel content (a `glTexImage2D` /
    /// `glTexSubImage2D` / `glCopyTexSubImage2D` upload) rather than a zero-filled STORAGE allocation. A
    /// `glTexImage2D(…, NULL)` or `glTexStorage2D` allocates a zeroed plane so a later `glTexSubImage*` has a
    /// target — but that plane is NOT content: it is exactly the shape an FBO color attachment takes before
    /// it is rendered into. `has_data()` (`!data.is_empty()`) is TRUE for such an attachment even though its
    /// real pixels live only in the FBO render target. `false` until a real upload lands; reset to `false` by a
    /// `glTexImage2D(…, NULL)` re-define (GL discards the old content). Sampling authority is tracked separately
    /// because a real CPU upload may be followed by a newer framebuffer render.
    pub(crate) real_pixels: bool,
    /// `true` when the latest accepted write to this generation was a framebuffer render.
    ///
    /// CPU uploads and GPU render targets are separate host resources in the neutral backend even though GL
    /// exposes one unified texture image. `real_pixels` only says that a CPU upload has happened; it cannot
    /// decide which resource is current after that image is subsequently rendered as an FBO attachment.
    pub(crate) gpu_authoritative: bool,
    /// Shared linear-EGLImage pixels. `shared_version` records the content observed by this GL view when it
    /// was bound or last mutated; a cloned deferred draw can therefore detect a newer sibling write.
    pub(crate) shared: Option<Arc<SharedPixels>>,
    pub(crate) shared_version: u64,
}

impl Default for GlTexture {
    fn default() -> Self {
        // GL's default sampler state: NEAREST_MIPMAP_LINEAR min, LINEAR mag, REPEAT wrap.
        Self {
            w: 0,
            h: 0,
            depth: 1,
            data: Arc::new(Vec::new()),
            cube_faces: std::array::from_fn(|_| None),
            cube_face_descs: [None; 6],
            cube_mip_descs: Vec::new(),
            layers: Vec::new(),
            min_filter: GL_NEAREST_MIPMAP_LINEAR,
            mag_filter: GL_LINEAR,
            wrap_s: GL_REPEAT,
            wrap_t: GL_REPEAT,
            swizzle: [GL_RED, GL_GREEN, GL_BLUE, GL_ALPHA],
            gen: 0,
            immutable: false,
            levels: 1,
            base_level: 0,
            max_level: 1000,
            mips: Vec::new(),
            internal_format: 0,
            ir_format: TextureFormat::Rgba8Unorm,
            real_pixels: false,
            gpu_authoritative: false,
            shared: None,
            shared_version: 0,
        }
    }
}

impl GlTexture {
    pub(crate) fn cube_faces(&self) -> &[Option<Arc<Vec<u8>>>; 6] {
        &self.cube_faces
    }
    pub(crate) fn layers(&self) -> &[Arc<Vec<u8>>] {
        &self.layers
    }
    pub fn layer(&self, layer: usize) -> Option<&[u8]> {
        if layer == 0 {
            Some(self.data.as_slice())
        } else {
            self.layers.get(layer - 1).map(|data| data.as_slice())
        }
    }
    /// The neutral min-filter for this texture's GL min-filter (`gl_shim.c`: Linear for LINEAR /
    /// LINEAR_MIPMAP_*, else Nearest).
    pub fn ir_min_filter(&self) -> Filter {
        match self.min_filter {
            GL_LINEAR | GL_LINEAR_MIPMAP_NEAREST | GL_LINEAR_MIPMAP_LINEAR => Filter::Linear,
            _ => Filter::Nearest,
        }
    }

    /// The neutral mag-filter (`gl_shim.c`: Linear only for exactly LINEAR).
    pub fn ir_mag_filter(&self) -> Filter {
        if self.mag_filter == GL_LINEAR {
            Filter::Linear
        } else {
            Filter::Nearest
        }
    }

    pub fn ir_mip_filter(&self) -> Filter {
        match self.min_filter {
            GL_NEAREST_MIPMAP_LINEAR | GL_LINEAR_MIPMAP_LINEAR => Filter::Linear,
            _ => Filter::Nearest,
        }
    }

    pub fn uses_mipmaps(&self) -> bool {
        !matches!(self.min_filter, GL_NEAREST | GL_LINEAR)
    }

    /// The neutral S wrap (`gl_shim.c`: ClampToEdge / MirrorRepeat / else Repeat).
    pub fn ir_wrap_s(&self) -> AddressMode {
        Self::address_mode(self.wrap_s)
    }

    /// The neutral T wrap.
    pub fn ir_wrap_t(&self) -> AddressMode {
        Self::address_mode(self.wrap_t)
    }

    /// Has this texture usable sampled content (materialized pixels)?
    pub fn has_data(&self) -> bool {
        !self.data.is_empty()
    }

    /// Bytes per texel of this texture's CPU shadow plane — a property of the plane, read from the format
    /// it materializes, not the constant four every site used to assume. See [`plane_bytes_per_texel`] for
    /// why four is still the floor.
    pub fn bytes_per_texel(&self) -> usize {
        plane_bytes_per_texel(self.ir_format)
    }

    /// Whether this plane is the four-byte-per-texel eight-bit image the RGBA8 pixel operations understand.
    ///
    /// The Chromium copy transforms, the box filter behind `glGenerateMipmap`, and the rect extractions
    /// that feed a copy back into a texture all read texels as four unsigned bytes. On a half-float or
    /// float plane that arithmetic is meaningless, so those paths ask this and decline rather than
    /// producing a plausible wrong image. Sizing the plane correctly is what makes the question answerable.
    pub fn is_rgba8_plane(&self) -> bool {
        self.bytes_per_texel() == 4
    }

    /// The CONTIGUOUS mip chain above the base level, stopping at the first level the application never
    /// uploaded. A host texture must have every level it declares, so a gap ends the chain rather than
    /// leaving a hole — and a chain whose extents do not halve is not a mip chain at all, so it is
    /// rejected here instead of being handed to the host as one.
    pub fn mip_chain(&self) -> &[MipLevel] {
        let mut count = 0;
        let (mut w, mut h) = (self.w, self.h);
        for level in &self.mips {
            let (want_w, want_h) = ((w / 2).max(1), (h / 2).max(1));
            if level.data.is_empty() || level.w != want_w || level.h != want_h {
                break;
            }
            count += 1;
            w = level.w;
            h = level.h;
        }
        &self.mips[..count]
    }

    /// The number of mip levels the host texture should declare: the base plus its contiguous chain,
    /// clamped by `GL_TEXTURE_MAX_LEVEL`.
    pub fn mip_levels(&self) -> u32 {
        self.effective_levels().len().max(1) as u32
    }

    /// The levels the host texture actually receives, as `(width, height, pixels)` from the EFFECTIVE base
    /// downwards — GL's `[GL_TEXTURE_BASE_LEVEL, GL_TEXTURE_MAX_LEVEL]` window.
    ///
    /// GL's base level RE-INDEXES the pyramid: with `GL_TEXTURE_BASE_LEVEL = 2` the texture *is* level 2
    /// and below, level 2 is what a magnifying draw samples, and levels 0 and 1 are not part of the texture
    /// at all. WebGPU has no base-level parameter, so the window is applied here by handing the host a
    /// pyramid that starts at the base level — which is the same thing, and needs nothing from the IR.
    ///
    /// Reporting the clamp back through `glGetTexParameteriv` while ignoring it for SAMPLING is how a
    /// magnifying draw under `base = 2` returned level 0: no LOD computation is involved in magnification,
    /// so there was no latitude to hide behind.
    pub fn effective_levels(&self) -> Vec<(i32, i32, Arc<Vec<u8>>)> {
        let mut all: Vec<(i32, i32, Arc<Vec<u8>>)> = Vec::with_capacity(1 + self.mip_chain().len());
        all.push((self.w, self.h, Arc::clone(&self.data)));
        for level in self.mip_chain() {
            all.push((level.w, level.h, Arc::clone(&level.data)));
        }
        let base = self.base_level.max(0) as usize;
        let last = self.max_level.max(0) as usize;
        if base >= all.len() || base > last {
            // A base level past the levels that exist makes the texture mipmap-INCOMPLETE. Keep the level
            // 0 image rather than handing the host an empty pyramid; completeness is a separate concern.
            return all.into_iter().take(1).collect();
        }
        let end = (last + 1).min(all.len());
        all[base..end].to_vec()
    }

    /// Host levels materialized for the active sampler's minification mode. A non-mipmapped sampler still
    /// needs its original LOD derivative so the GPU can choose between GL's min and mag filters; collapsing
    /// the sampler LOD range to zero destroys that choice. Instead expose only the effective base image.
    pub fn sampled_levels(&self, uses_mipmaps: bool) -> Vec<(i32, i32, Arc<Vec<u8>>)> {
        let mut levels = self.effective_levels();
        if !uses_mipmaps {
            levels.truncate(1);
        }
        levels
    }

    /// Whether a cube sampler may use this texture. ES 2.0 requires all six faces at every required
    /// level, square and identically sized/formatted, with a complete halving chain when the min filter
    /// uses mipmaps. Incomplete sampling returns opaque black; lowering materializes that fallback.
    pub fn cube_complete(&self, uses_mipmaps: bool) -> bool {
        let Some(base) = self.cube_face_descs[0] else {
            return false;
        };
        if base.w <= 0
            || base.w != base.h
            || self.cube_face_descs.iter().any(|face| *face != Some(base))
        {
            return false;
        }
        if !uses_mipmaps {
            return true;
        }
        let required_levels = (i32::BITS - base.w.max(1).leading_zeros()) as usize;
        for level in 1..required_levels {
            let extent = (base.w >> level).max(1);
            let expected = CubeImageDesc {
                w: extent,
                h: extent,
                internal_format: base.internal_format,
            };
            let Some(faces) = self.cube_mip_descs.get(level - 1) else {
                return false;
            };
            if faces.iter().any(|face| *face != Some(expected)) {
                return false;
            }
        }
        true
    }

    /// Has this texture received a REAL pixel upload (`glTexImage2D`-with-pixels / `glTexSubImage2D` /
    /// `glCopyTexSubImage2D`), as opposed to only a zero-filled STORAGE allocation (`glTexImage2D(…, NULL)` /
    /// `glTexStorage2D`)? A texture that is an FBO color attachment allocated with NULL storage has a zeroed
    /// `data` plane (`has_data()` true) but NO real content — its pixels live in the FBO render target — so
    /// the frame builder samples the resident render target rather than the zeroed plane when this is `false`.
    pub fn has_real_pixels(&self) -> bool {
        self.real_pixels
    }

    /// Whether sampling must use the resident framebuffer target rather than the CPU shadow.
    pub fn gpu_authoritative(&self) -> bool {
        self.gpu_authoritative
    }

    pub fn shared_version(&self) -> u64 {
        self.shared.as_ref().map_or(0, |storage| storage.version())
    }

    /// Resolve the newest shared-image pixels into this local view. Returns whether content advanced since
    /// this view (or deferred draw snapshot) last observed the storage.
    pub fn resolve_shared(&mut self) -> bool {
        let Some(storage) = self.shared.as_ref() else {
            return false;
        };
        let snapshot = storage.snapshot();
        if snapshot.version == self.shared_version {
            return false;
        }
        self.data = snapshot.data;
        self.real_pixels = !self.data.is_empty();
        self.shared_version = snapshot.version;
        true
    }

    /// A cache generation that distinguishes GL object redefinitions from shared sibling content writes.
    pub fn sampled_generation(&self) -> (u64, u64) {
        (self.gen, self.shared_version())
    }

    /// Stable identity of the shared storage revision observed by this view.
    pub(crate) fn shared_identity(&self) -> Option<(u64, u64)> {
        self.shared
            .as_ref()
            .map(|storage| (storage.id(), self.shared_version))
    }

    pub(crate) fn shared_current_identity(&self) -> Option<(u64, u64)> {
        self.shared
            .as_ref()
            .map(|storage| (storage.id(), storage.version()))
    }

    pub(crate) fn shared_storage(&self) -> Option<u64> {
        self.shared.as_ref().map(|storage| storage.id())
    }

    pub(crate) fn shared_residency(&self) -> Option<(u64, u64, Weak<SharedPixels>)> {
        self.shared
            .as_ref()
            .map(|storage| (storage.id(), self.shared_version, Arc::downgrade(storage)))
    }
}

impl GlTexture {
    fn address_mode(gl: u32) -> AddressMode {
        match gl {
            GL_CLAMP_TO_EDGE => AddressMode::ClampToEdge,
            GL_MIRRORED_REPEAT => AddressMode::MirrorRepeat,
            _ => AddressMode::Repeat,
        }
    }
}

/// The per-context texture table: GL name → [`GlTexture`], with a monotonic name counter. Name `0` is
/// the reserved "no texture" binding.
/// No `Default`: the derived one would start `next_name` at `0`, and `0` is the reserved "no texture"
/// binding, so the first `glGenTextures` through it would hand the application a name that means "unbind"
/// and every upload would land on the default texture. `next_generation` has the same problem — `0` is
/// the generation a never-uploaded texture carries. [`Textures::new`] is the only way to build one.
#[derive(Debug)]
pub struct Textures {
    map: HashMap<u32, GlTexture>,
    /// Stable object identities for the live public names in `map`.
    ///
    /// GL object names are reusable. An FBO attachment retains the object, not the spelling of the name,
    /// after `glDeleteTextures`; `retired` therefore keeps objects whose public name has already become
    /// available to a distinct texture.
    objects: HashMap<u32, u64>,
    live_objects: HashMap<u64, u32>,
    retired: HashMap<u64, (u32, GlTexture)>,
    next_name: u32,
    next_object: u64,
    next_generation: u64,
}

impl Textures {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            objects: HashMap::new(),
            live_objects: HashMap::new(),
            retired: HashMap::new(),
            next_name: 1,
            next_object: 1,
            next_generation: 1,
        }
    }

    fn materialize(&mut self, name: u32) {
        if self.map.contains_key(&name) {
            return;
        }
        let object = self.next_object;
        self.next_object = self.next_object.saturating_add(1);
        self.map.insert(name, GlTexture::default());
        self.objects.insert(name, object);
        self.live_objects.insert(object, name);
    }

    pub(crate) fn shared_residency(&self, storage: u64) -> Option<(u64, Weak<SharedPixels>)> {
        self.map.values().find_map(|texture| {
            let (candidate, _revision, residency) = texture.shared_residency()?;
            (candidate == storage)
                .then(|| residency.upgrade())
                .flatten()
                .map(|shared| (shared.version(), Arc::downgrade(&shared)))
        })
    }

    fn generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        generation
    }

    /// `glGenTextures` — mint a fresh GL texture name.
    pub fn gen(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        self.materialize(name);
        name
    }

    /// Materialize a non-zero name bound through `GL_CHROMIUM_bind_generates_resource`.
    pub fn ensure(&mut self, name: u32) {
        if name != 0 {
            self.materialize(name);
            self.next_name = self.next_name.max(name.saturating_add(1));
        }
    }

    /// Stable identity of the texture object currently named by `name`.
    pub fn object(&self, name: u32) -> Option<u64> {
        self.objects.get(&name).copied()
    }

    /// Resolve an attachment's stable object identity, including an object whose public name was deleted
    /// and subsequently reused for a different texture.
    pub fn get_object(&self, object: u64) -> Option<&GlTexture> {
        self.live_objects
            .get(&object)
            .and_then(|name| self.map.get(name))
            .or_else(|| self.retired.get(&object).map(|(_, texture)| texture))
    }

    pub fn get_object_mut(&mut self, object: u64) -> Option<&mut GlTexture> {
        if let Some(name) = self.live_objects.get(&object).copied() {
            return self.map.get_mut(&name);
        }
        self.retired.get_mut(&object).map(|(_, texture)| texture)
    }

    /// Remove `name` from the public namespace while retaining its object for container references.
    pub fn retire_name(&mut self, name: u32) -> bool {
        let Some(texture) = self.map.remove(&name) else {
            return false;
        };
        let Some(object) = self.objects.remove(&name) else {
            self.map.insert(name, texture);
            return false;
        };
        self.live_objects.remove(&object);
        self.retired.insert(object, (name, texture));
        true
    }

    /// Drop an object after its last container reference has gone.
    pub fn delete_object(&mut self, object: u64) -> bool {
        self.retired.remove(&object).is_some()
    }

    pub(crate) fn take_retired_object(&mut self, object: u64) -> Option<(u32, GlTexture)> {
        self.retired.remove(&object)
    }

    /// Copy one texture's base-level RGBA8 plane into another, applying Chromium's upload transforms.
    pub fn copy(
        &mut self,
        source: u32,
        destination: u32,
        flip_y: bool,
        premultiply: bool,
        unmultiply: bool,
    ) -> bool {
        let Some(mut source) = self.map.get(&source).cloned() else {
            return false;
        };
        source.resolve_shared();
        // `transform_rgba` reads texels as four unsigned bytes, so this operation is defined only over an
        // RGBA8 plane. Refusing a wider one is not a new restriction — the length test below already
        // excluded it, since a float plane's length is a multiple of its own texel — but stating it by the
        // plane's texel says why, and keeps saying it if the length test is ever loosened.
        if !source.is_rgba8_plane()
            || source.data.len() != source.w.max(0) as usize * source.h.max(0) as usize * 4
        {
            return false;
        }
        let mut pixels = (*source.data).clone();
        transform_rgba(
            &mut pixels,
            source.w as usize,
            source.h as usize,
            flip_y,
            premultiply,
            unmultiply,
        );
        let generation = self.generation();
        let destination = self.map.entry(destination).or_default();
        destination.w = source.w;
        destination.h = source.h;
        destination.data = Arc::new(pixels);
        destination.ir_format = source.ir_format;
        destination.real_pixels = true;
        destination.gpu_authoritative = false;
        destination.gen = generation;
        if let Some(shared) = &destination.shared {
            destination.shared_version = shared.store(destination.data.clone());
        }
        true
    }

    /// Copy a source rectangle into an existing destination texture.
    #[allow(clippy::too_many_arguments)]
    pub fn copy_sub(
        &mut self,
        source: u32,
        destination: u32,
        source_x: i32,
        source_y: i32,
        destination_x: i32,
        destination_y: i32,
        width: i32,
        height: i32,
        flip_y: bool,
        premultiply: bool,
        unmultiply: bool,
    ) -> bool {
        if [
            source_x,
            source_y,
            destination_x,
            destination_y,
            width,
            height,
        ]
        .iter()
        .any(|value| *value < 0)
        {
            return false;
        }
        let Some(mut source) = self.map.get(&source).cloned() else {
            return false;
        };
        source.resolve_shared();
        let (sx, sy, dx, dy, width, height) = (
            source_x as usize,
            source_y as usize,
            destination_x as usize,
            destination_y as usize,
            width as usize,
            height as usize,
        );
        let (source_width, source_height) = (source.w.max(0) as usize, source.h.max(0) as usize);
        if !source.is_rgba8_plane()
            || sx + width > source_width
            || sy + height > source_height
            || source.data.len() != source_width * source_height * 4
        {
            return false;
        }
        let mut pixels = vec![0; width * height * 4];
        for row in 0..height {
            let start = ((sy + row) * source_width + sx) * 4;
            pixels[row * width * 4..(row + 1) * width * 4]
                .copy_from_slice(&source.data[start..start + width * 4]);
        }
        transform_rgba(&mut pixels, width, height, flip_y, premultiply, unmultiply);
        self.sub_image_2d(
            destination,
            dx as i32,
            dy as i32,
            width as i32,
            height as i32,
            &pixels,
        )
    }

    /// `glTexImage2D` — (re)define the texture's plane + extent + neutral texel `format`, bumping its
    /// generation. `pixels` is the already-converted image in the texel format `format` names
    /// (`w * h * plane_bytes_per_texel(format)` bytes), or empty for a storage-only define (e.g. an FBO
    /// color attachment allocated before it is rendered into).
    ///
    /// Returns whether the define was accepted. Supplied pixels of any other size are REFUSED, and the
    /// texture is left with the zeroed plane its declared format names rather than with an image whose
    /// bytes disagree with the format beside them — the disagreement every other site in this model was
    /// just taught not to make. This is the fourth of the four allocation paths, and it is the one that
    /// takes content: `glTexStorage2D`, a storage-only `glTexImage2D` and `glRenderbufferStorage` all
    /// allocate and never fill, which is why they could honour a wide plane before the upload could.
    pub fn image_2d(
        &mut self,
        name: u32,
        w: i32,
        h: i32,
        pixels: &[u8],
        format: TextureFormat,
    ) -> bool {
        // The zeroed-storage byte size, computed in usize with a checked multiply so a large (or
        // adversarial) w*h never overflows an i32 and panics — an out-of-range extent saturates to a
        // no-alloc `None` rather than crashing the driver (the record layer raises GL_INVALID_VALUE).
        let want = (w.max(0) as usize)
            .checked_mul(h.max(0) as usize)
            .and_then(|n| n.checked_mul(plane_bytes_per_texel(format)));
        let generation = self.generation();
        let t = self.map.entry(name).or_default();
        t.shared = None;
        t.shared_version = 0;
        t.w = w;
        t.h = h;
        t.ir_format = format;
        let mismatched = !pixels.is_empty() && Some(pixels.len()) != want;
        if !pixels.is_empty() && !mismatched {
            t.data = Arc::new(pixels.to_vec());
            t.real_pixels = true;
        } else if let Some(want) = want {
            if t.data.len() != want {
                t.data = Arc::new(vec![0u8; want]);
            }
            // A NULL-storage (re)define carries no content — GL discards any prior pixels — so the plane is
            // no longer real content (it is the pre-render shape of an FBO color attachment).
            t.real_pixels = false;
        }
        t.gpu_authoritative = false;
        t.gen = generation;
        !mismatched
    }

    /// Define one base-level cube face while retaining the other five faces of the same texture object.
    pub fn image_cube_face(
        &mut self,
        name: u32,
        face: usize,
        w: i32,
        h: i32,
        pixels: &[u8],
        format: TextureFormat,
        internal_format: u32,
    ) -> bool {
        if face >= 6 {
            return false;
        }
        let accepted = self.image_2d(name, w, h, pixels, format);
        if accepted {
            let t = self
                .map
                .get_mut(&name)
                .expect("image_2d materialized texture");
            t.cube_faces[face] = Some(Arc::clone(&t.data));
            t.cube_face_descs[face] = Some(CubeImageDesc {
                w,
                h,
                internal_format,
            });
        }
        accepted
    }

    /// `glTexImage2D` at a level ABOVE the base — one image of a mip chain. Stored beside the base image
    /// (see [`GlTexture::mips`]) rather than replacing it, and bumps the generation so the resident host
    /// texture is rebuilt with the new level count.
    ///
    /// A level far past any plausible chain is ignored rather than allowed to allocate: the level index is
    /// application input, and `1 << level` is the extent it implies.
    pub fn image_2d_level(&mut self, name: u32, level: u32, w: i32, h: i32, pixels: &[u8]) -> bool {
        const MAX_LEVELS: usize = 16;
        if level == 0 || level as usize > MAX_LEVELS || w <= 0 || h <= 0 {
            return false;
        }
        let generation = self.generation();
        let Some(t) = self.map.get_mut(&name) else {
            return false;
        };
        let index = level as usize - 1;
        if t.mips.len() <= index {
            t.mips.resize(index + 1, MipLevel::default());
        }
        t.mips[index] = MipLevel {
            w,
            h,
            depth: 1,
            data: Arc::new(pixels.to_vec()),
            layers: Vec::new(),
        };
        t.gen = generation;
        true
    }

    /// Define one non-base mip level of one cube face while retaining every sibling face.
    pub fn image_cube_level(
        &mut self,
        name: u32,
        face: usize,
        level: u32,
        w: i32,
        h: i32,
        pixels: &[u8],
        internal_format: u32,
    ) -> bool {
        const MAX_LEVELS: usize = 16;
        if face >= 6 || level == 0 || level as usize > MAX_LEVELS || w <= 0 || h <= 0 {
            return false;
        }
        let generation = self.generation();
        let Some(texture) = self.map.get_mut(&name) else {
            return false;
        };
        let index = level as usize - 1;
        if texture.mips.len() <= index {
            texture.mips.resize(index + 1, MipLevel::default());
        }
        let mip = &mut texture.mips[index];
        mip.w = w;
        mip.h = h;
        mip.depth = 6;
        if mip.layers.len() < 5 {
            mip.layers.resize_with(5, || Arc::new(Vec::new()));
        }
        if face == 0 {
            mip.data = Arc::new(pixels.to_vec());
        } else {
            mip.layers[face - 1] = Arc::new(pixels.to_vec());
        }
        texture.gen = generation;
        if texture.cube_mip_descs.len() <= index {
            texture.cube_mip_descs.resize(index + 1, [None; 6]);
        }
        texture.cube_mip_descs[index][face] = Some(CubeImageDesc {
            w,
            h,
            internal_format,
        });
        true
    }

    pub fn image_3d_level(
        &mut self,
        name: u32,
        level: u32,
        w: i32,
        h: i32,
        depth: i32,
        pixels: &[u8],
    ) -> bool {
        const MAX_LEVELS: usize = 16;
        if level == 0 || level as usize > MAX_LEVELS || w <= 0 || h <= 0 || depth <= 0 {
            return false;
        }
        let generation = self.generation();
        let Some(t) = self.map.get_mut(&name) else {
            return false;
        };
        let Some(plane) = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(t.bytes_per_texel()))
        else {
            return false;
        };
        let Some(total) = plane.checked_mul(depth as usize) else {
            return false;
        };
        if pixels.len() < total {
            return false;
        }
        let index = level as usize - 1;
        if t.mips.len() <= index {
            t.mips.resize(index + 1, MipLevel::default());
        }
        t.mips[index] = MipLevel {
            w,
            h,
            depth,
            data: Arc::new(pixels[..plane].to_vec()),
            layers: pixels[plane..total]
                .chunks_exact(plane)
                .map(|slice| Arc::new(slice.to_vec()))
                .collect(),
        };
        t.gen = generation;
        true
    }

    /// Define host-external storage without allocating or reading a CPU shadow plane.
    pub fn external_image_2d(&mut self, name: u32, w: i32, h: i32, format: TextureFormat) {
        let generation = self.generation();
        let texture = self.map.entry(name).or_default();
        texture.shared = None;
        texture.shared_version = 0;
        texture.w = w;
        texture.h = h;
        texture.ir_format = format;
        // An import REDEFINES the texture, so the format the guest declared for whatever this name held
        // before must go with it. Leaving it behind made completeness judge an imported colour buffer by
        // its predecessor: allocate a backend texture with a sized format, wrap a shared image over it,
        // and the framebuffer is incomplete for a format the import replaced. Zero is the unsized
        // spelling — the right answer here, because an imported image is a colour buffer by construction
        // and the guest never declared a sized format for it; EGL did.
        texture.internal_format = 0;
        texture.data = Arc::new(Vec::new());
        texture.real_pixels = false;
        texture.gpu_authoritative = false;
        texture.gen = generation;
    }

    /// `glTexStorage2D`/`glTexStorage3D` — allocate immutable zeroed storage for the plane `format` names,
    /// mark the texture immutable, and record the declared `levels`. Mirrors `gl_shim.c`, which allocates
    /// the base plane so a later `glTexSubImage*` has a target (levels are not otherwise materialized).
    /// Returns `false` if the byte size overflows.
    pub fn storage_2d(
        &mut self,
        name: u32,
        w: i32,
        h: i32,
        levels: i32,
        format: TextureFormat,
        internal_format: u32,
    ) -> bool {
        let Some(size) = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(plane_bytes_per_texel(format)))
        else {
            return false;
        };
        let generation = self.generation();
        let t = self.map.entry(name).or_default();
        t.shared = None;
        t.shared_version = 0;
        t.w = w;
        t.h = h;
        t.ir_format = format;
        t.internal_format = internal_format;
        t.data = Arc::new(vec![0u8; size]);
        t.immutable = true;
        t.levels = levels;
        // Zeroed immutable storage is not real content until a `glTexSubImage*` fills it (an FBO attachment
        // allocated this way renders its pixels into the render target, not this plane).
        t.real_pixels = false;
        t.gpu_authoritative = false;
        t.gen = generation;
        true
    }

    /// Allocate (or grow to) the layer-0 base plane for a 2D-array / 3D texture upload
    /// (`glTexImage3D`/`glTexStorage3D`) — `gl_shim.c` materializes only the layer-0 plane. Returns
    /// `false` on a size overflow.
    ///
    /// The plane is sized by the format the texture already declares rather than by a fixed four bytes, so
    /// an array or 3D texture whose storage was declared as a float format gets the plane that format
    /// names. This path does not itself choose a format: `glTexImage3D` carries an `internalformat` this
    /// model does not yet plumb through, so a texture that has never declared one keeps the default
    /// `Rgba8Unorm` and the four-byte plane it always had.
    pub fn alloc_plane(&mut self, name: u32, w: i32, h: i32) -> bool {
        let texel = self.map.get(&name).map_or(4, GlTexture::bytes_per_texel);
        let Some(size) = (w.max(0) as usize)
            .checked_mul(h.max(0) as usize)
            .and_then(|n| n.checked_mul(texel))
        else {
            return false;
        };
        let generation = self.generation();
        let t = self.map.entry(name).or_default();
        t.shared = None;
        t.shared_version = 0;
        t.w = w;
        t.h = h;
        t.depth = 1;
        t.layers.clear();
        if t.data.len() != size {
            t.data = Arc::new(vec![0u8; size]);
        }
        t.real_pixels = false;
        t.gpu_authoritative = false;
        t.gen = generation;
        true
    }

    pub fn alloc_volume(&mut self, name: u32, w: i32, h: i32, depth: i32, pixels: &[u8]) -> bool {
        if depth <= 0 || !self.alloc_plane(name, w, h) {
            return false;
        }
        let Some(texture) = self.map.get_mut(&name) else {
            return false;
        };
        let plane = texture.data.len();
        let Some(total) = plane.checked_mul(depth as usize) else {
            return false;
        };
        if !pixels.is_empty() && pixels.len() < total {
            return false;
        }
        texture.depth = depth;
        if pixels.is_empty() {
            texture.data = Arc::new(vec![0; plane]);
            texture.layers = (1..depth).map(|_| Arc::new(vec![0; plane])).collect();
            texture.real_pixels = false;
        } else {
            texture.data = Arc::new(pixels[..plane].to_vec());
            texture.layers = pixels[plane..total]
                .chunks_exact(plane)
                .map(|slice| Arc::new(slice.to_vec()))
                .collect();
            texture.real_pixels = true;
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sub_image_3d(
        &mut self,
        name: u32,
        xo: i32,
        yo: i32,
        zo: i32,
        w: i32,
        h: i32,
        depth: i32,
        pixels: &[u8],
    ) -> bool {
        let Some(texture) = self.map.get(&name) else {
            return false;
        };
        if zo < 0 || depth <= 0 || zo.checked_add(depth).is_none_or(|end| end > texture.depth) {
            return false;
        }
        let Some(plane_bytes) = (w.max(0) as usize)
            .checked_mul(h.max(0) as usize)
            .and_then(|n| n.checked_mul(texture.bytes_per_texel()))
        else {
            return false;
        };
        if pixels.len() < plane_bytes.saturating_mul(depth as usize) {
            return false;
        }
        for layer in 0..depth {
            let z = (zo + layer) as usize;
            if z == 0 {
                if !self.sub_image_2d(
                    name,
                    xo,
                    yo,
                    w,
                    h,
                    &pixels[layer as usize * plane_bytes..][..plane_bytes],
                ) {
                    return false;
                }
            } else {
                let Some(texture) = self.map.get_mut(&name) else {
                    return false;
                };
                let texel = texture.bytes_per_texel();
                let tw = texture.w as usize;
                let Some(target) = texture.layers.get_mut(z - 1) else {
                    return false;
                };
                let mut owned = (**target).clone();
                for row in 0..h as usize {
                    let dst = ((yo as usize + row) * tw + xo as usize) * texel;
                    let src = layer as usize * plane_bytes + row * w as usize * texel;
                    owned[dst..dst + w as usize * texel]
                        .copy_from_slice(&pixels[src..src + w as usize * texel]);
                }
                *target = Arc::new(owned);
            }
        }
        let generation = self.generation();
        if let Some(texture) = self.map.get_mut(&name) {
            texture.real_pixels = true;
            texture.gpu_authoritative = false;
            texture.gen = generation;
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sub_image_3d_level(
        &mut self,
        name: u32,
        level: u32,
        xo: i32,
        yo: i32,
        zo: i32,
        w: i32,
        h: i32,
        depth: i32,
        pixels: &[u8],
    ) -> bool {
        if level == 0 {
            return self.sub_image_3d(name, xo, yo, zo, w, h, depth, pixels);
        }
        let generation = self.generation();
        let Some(texture) = self.map.get_mut(&name) else {
            return false;
        };
        let texel = texture.bytes_per_texel();
        let Some(mip) = texture.mips.get_mut(level as usize - 1) else {
            return false;
        };
        if xo < 0
            || yo < 0
            || zo < 0
            || w < 0
            || h < 0
            || depth <= 0
            || xo.checked_add(w).is_none_or(|end| end > mip.w)
            || yo.checked_add(h).is_none_or(|end| end > mip.h)
            || zo.checked_add(depth).is_none_or(|end| end > mip.depth)
        {
            return false;
        }
        let Some(plane_bytes) = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(texel))
        else {
            return false;
        };
        if pixels.len() < plane_bytes.saturating_mul(depth as usize) {
            return false;
        }
        for layer in 0..depth {
            let z = (zo + layer) as usize;
            let target = if z == 0 {
                &mut mip.data
            } else {
                let Some(target) = mip.layers.get_mut(z - 1) else {
                    return false;
                };
                target
            };
            let mut owned = (**target).clone();
            for row in 0..h as usize {
                let dst = ((yo as usize + row) * mip.w as usize + xo as usize) * texel;
                let src = layer as usize * plane_bytes + row * w as usize * texel;
                owned[dst..dst + w as usize * texel]
                    .copy_from_slice(&pixels[src..src + w as usize * texel]);
            }
            *target = Arc::new(owned);
        }
        texture.real_pixels = true;
        texture.gpu_authoritative = false;
        texture.gen = generation;
        true
    }

    /// `glTexSubImage2D` / `glCopyTexSubImage2D` — overwrite the `[xo,xo+w) × [yo,yo+h)` sub-rect of an
    /// existing texture's plane with `pixels`: `w * h * `[`GlTexture::bytes_per_texel`] tightly-packed
    /// bytes, in the DESTINATION plane's texel format. Silently clips to the texture bounds (the caller
    /// validates the range first). Returns `false` for an unknown/dataless name, and for a `pixels` shorter
    /// than the rect the destination plane's own texel requires.
    ///
    /// That last refusal is the fix for a silent corruption. The stride was a hardcoded four bytes per
    /// texel on both sides, so an upload carrying four-byte texels into a half-float plane satisfied every
    /// check, wrote half the rect, and returned success. Deriving the stride from the destination makes a
    /// caller that supplies the wrong texel size fail the length test instead of half-filling the plane.
    pub fn sub_image_2d(
        &mut self,
        name: u32,
        xo: i32,
        yo: i32,
        w: i32,
        h: i32,
        pixels: &[u8],
    ) -> bool {
        let Some(texture) = self.map.get(&name) else {
            return false;
        };
        if texture.data.is_empty() || w <= 0 || h <= 0 || xo < 0 || yo < 0 {
            return false;
        }
        let texel = texture.bytes_per_texel();
        let (tw, th) = (texture.w as usize, texture.h as usize);
        let (xo, yo, w, h) = (xo as usize, yo as usize, w as usize, h as usize);
        let Some(row_bytes) = w.checked_mul(texel) else {
            return false;
        };
        let Some(upload_bytes) = row_bytes.checked_mul(h) else {
            return false;
        };
        // The destination plane must be exactly the size its extent and texel require, or the row writes
        // below index past its end. Every allocation path produces that size; a plane that does not have it
        // was filled by something that disagreed with the texture's own format, and refusing is the only
        // answer available here — the row write would panic and the alternative, clamping, is the
        // half-filled plane this whole change exists to remove.
        if tw.checked_mul(th).and_then(|n| n.checked_mul(texel)) != Some(texture.data.len()) {
            return false;
        }
        if xo.checked_add(w).is_none_or(|end| end > tw)
            || yo.checked_add(h).is_none_or(|end| end > th)
            || pixels.len() < upload_bytes
        {
            return false;
        }

        // An identical upload into an already CPU-authoritative private image changes no observable GL
        // state. Preserve the immutable backing and content generation so deferred draws before and after
        // the call share one resident upload instead of cloning and transmitting the complete texture plane.
        //
        // A GPU-authoritative texture is intentionally excluded: even bytes equal to its stale CPU shadow
        // represent a real host-texture update. Shared images are excluded because the upload is also a
        // storage publication observed by sibling views.
        let unchanged = texture.real_pixels
            && !texture.gpu_authoritative
            && texture.shared.is_none()
            && (0..h).all(|row| {
                let dst = ((yo + row) * tw + xo) * texel;
                let src = row * row_bytes;
                texture.data[dst..dst + row_bytes] == pixels[src..src + row_bytes]
            });
        if unchanged {
            return true;
        }

        let generation = self.generation();
        let Some(texture) = self.map.get_mut(&name) else {
            return false;
        };
        for row in 0..h {
            let dst = ((yo + row) * tw + xo) * texel;
            let src = row * row_bytes;
            Arc::make_mut(&mut texture.data)[dst..dst + row_bytes]
                .copy_from_slice(&pixels[src..src + row_bytes]);
        }
        texture.real_pixels = true;
        texture.gpu_authoritative = false;
        texture.gen = generation;
        if let Some(shared) = &texture.shared {
            texture.shared_version = shared.store(texture.data.clone());
        }
        true
    }

    /// Attach one GL texture view to shared linear-EGLImage storage without changing its object generation
    /// or sampler state.
    pub fn bind_shared(&mut self, name: u32, shared: Arc<SharedPixels>) -> bool {
        let Some(texture) = self.map.get_mut(&name) else {
            return false;
        };
        let snapshot = shared.snapshot();
        texture.data = snapshot.data;
        texture.real_pixels = !texture.data.is_empty();
        texture.gpu_authoritative = false;
        texture.shared_version = snapshot.version;
        texture.shared = Some(shared);
        true
    }

    /// Mark an accepted framebuffer write as the current content for this exact texture generation.
    ///
    /// The generation check prevents a completed batch for an older definition from overriding a newer CPU
    /// upload. Call this only after the command sink accepts the render batch.
    pub fn mark_rendered(&mut self, name: u32, generation: u64) {
        if let Some(texture) = self.map.get_mut(&name) {
            if texture.gen == generation {
                texture.gpu_authoritative = true;
            }
        }
    }

    pub fn get_mut(&mut self, name: u32) -> Option<&mut GlTexture> {
        self.map.get_mut(&name)
    }

    /// `glTexParameteri` — set one filter/wrap parameter.
    pub fn set_param(&mut self, name: u32, pname: u32, value: u32) {
        let generation = self.generation();
        let mut bump = false;
        if let Some(t) = self.map.get_mut(&name) {
            match pname {
                GL_TEXTURE_MIN_FILTER => t.min_filter = value,
                GL_TEXTURE_MAG_FILTER => t.mag_filter = value,
                GL_TEXTURE_WRAP_S => t.wrap_s = value,
                GL_TEXTURE_WRAP_T => t.wrap_t = value,
                GL_TEXTURE_SWIZZLE_R => t.swizzle[0] = value,
                GL_TEXTURE_SWIZZLE_G => t.swizzle[1] = value,
                GL_TEXTURE_SWIZZLE_B => t.swizzle[2] = value,
                GL_TEXTURE_SWIZZLE_A => t.swizzle[3] = value,
                // These change WHICH levels the host texture must carry, so the resident upload is stale:
                // bump the generation exactly as a pixel write does, or the re-based pyramid is never sent.
                GL_TEXTURE_BASE_LEVEL => {
                    t.base_level = value as i32;
                    bump = true;
                }
                GL_TEXTURE_MAX_LEVEL => {
                    t.max_level = value as i32;
                    bump = true;
                }
                _ => {}
            }
            if bump {
                t.gen = generation;
            }
        }
    }

    /// Set the four-component `GL_TEXTURE_SWIZZLE_RGBA` vector.
    pub fn set_swizzle(&mut self, name: u32, values: [u32; 4]) {
        if let Some(texture) = self.map.get_mut(&name) {
            texture.swizzle = values;
        }
    }

    pub fn get(&self, name: u32) -> Option<&GlTexture> {
        self.map.get(&name)
    }

    /// `glDeleteTextures` — drop the object.
    pub fn delete(&mut self, name: u32) -> bool {
        if let Some(object) = self.objects.remove(&name) {
            self.live_objects.remove(&object);
        }
        self.map.remove(&name).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

fn transform_rgba(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    flip_y: bool,
    premultiply: bool,
    unmultiply: bool,
) {
    if flip_y {
        let stride = width * 4;
        for row in 0..height / 2 {
            let opposite = height - row - 1;
            let (head, tail) = pixels.split_at_mut(opposite * stride);
            head[row * stride..(row + 1) * stride].swap_with_slice(&mut tail[..stride]);
        }
    }
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if premultiply {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
            }
        } else if unmultiply && alpha != 0 {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod sampled_level_tests {
    use super::*;

    fn three_level_texture() -> GlTexture {
        GlTexture {
            w: 4,
            h: 4,
            data: Arc::new(vec![1; 4 * 4 * 4]),
            mips: vec![
                MipLevel {
                    w: 2,
                    h: 2,
                    depth: 1,
                    data: Arc::new(vec![2; 2 * 2 * 4]),
                    layers: Vec::new(),
                },
                MipLevel {
                    w: 1,
                    h: 1,
                    depth: 1,
                    data: Arc::new(vec![3; 4]),
                    layers: Vec::new(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn non_mipmapped_sampling_materializes_only_the_base_image() {
        let levels = three_level_texture().sampled_levels(false);
        assert_eq!(levels.len(), 1);
        assert_eq!((levels[0].0, levels[0].1), (4, 4));
    }

    #[test]
    fn mipmapped_sampling_keeps_the_complete_pyramid() {
        let levels = three_level_texture().sampled_levels(true);
        assert_eq!(
            levels.iter().map(|(w, h, _)| (*w, *h)).collect::<Vec<_>>(),
            [(4, 4), (2, 2), (1, 1),]
        );
    }

    fn complete_cube(size: i32) -> GlTexture {
        let base = CubeImageDesc {
            w: size,
            h: size,
            internal_format: GL_RGBA,
        };
        let mut texture = GlTexture {
            w: size,
            h: size,
            data: Arc::new(vec![0; size as usize * size as usize * 4]),
            cube_face_descs: [Some(base); 6],
            ..Default::default()
        };
        let mut extent = size / 2;
        while extent > 0 {
            let level = CubeImageDesc {
                w: extent,
                h: extent,
                internal_format: GL_RGBA,
            };
            texture.cube_mip_descs.push([Some(level); 6]);
            extent /= 2;
        }
        texture
    }

    #[test]
    fn cube_requires_all_six_square_matching_base_faces() {
        let mut texture = complete_cube(4);
        assert!(texture.cube_complete(false));
        texture.cube_face_descs[3] = None;
        assert!(!texture.cube_complete(false));

        let mut texture = complete_cube(4);
        texture.cube_face_descs[2] = Some(CubeImageDesc {
            w: 4,
            h: 3,
            internal_format: GL_RGBA,
        });
        assert!(!texture.cube_complete(false));

        let mut texture = complete_cube(4);
        texture.cube_face_descs[5] = Some(CubeImageDesc {
            w: 4,
            h: 4,
            internal_format: GL_RGB,
        });
        assert!(!texture.cube_complete(false));
    }

    #[test]
    fn cube_mipmap_completeness_requires_every_face_of_the_halving_chain() {
        let mut texture = complete_cube(4);
        assert!(texture.cube_complete(true));
        texture.cube_mip_descs[0][4] = None;
        assert!(!texture.cube_complete(true));

        let mut texture = complete_cube(4);
        texture.cube_mip_descs[1][0] = Some(CubeImageDesc {
            w: 2,
            h: 2,
            internal_format: GL_RGBA,
        });
        assert!(!texture.cube_complete(true));
        assert!(texture.cube_complete(false));
    }
}
