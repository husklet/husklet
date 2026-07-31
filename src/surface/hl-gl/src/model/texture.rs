//! GL texture objects + the texture table: `glGenTextures`/`glTexImage2D`/`glTexParameteri` tracking.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Texture`, the `MAXTEX` array) + the resource lowering in
//! `hl-shim-gl/src/lower.rs`. A GL texture keeps its RGBA8 pixels (converted from the app's upload
//! format) + filter/wrap state; the frame builder ([`crate::service::frame`]) lowers a sampled texture
//! to a `CreateTexture` + `CreateSampler` + a staging `CreateBuffer`/`WriteBuffer` + a
//! `CopyBufferToTexture`, exactly as `gl_shim.c` does at swap.

use super::glconst::*;
use hl_gpu::protocol::model::enums::{AddressMode, Filter, TextureFormat};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};

static NEXT_SHARED_PIXELS_ID: AtomicU64 = AtomicU64::new(1);

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

/// One live GL texture object: RGBA8 pixels + the min/mag filter + S/T wrap GL enums.
#[derive(Clone, PartialEq, Debug)]
pub struct GlTexture {
    pub w: i32,
    pub h: i32,
    /// RGBA8 pixels (`w*h*4` bytes), converted from the app's `glTexImage2D` upload format.
    pub data: Arc<Vec<u8>>,
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
    ///
    /// This model stores ONE image per texture (the base level), so these do not yet select a level to
    /// sample; they are real, queryable state an application sets and reads back, and reporting the
    /// initial `0` / `1000` for whatever it had set made `glGetTexParameteriv` disagree with the value
    /// the app had just written.
    pub base_level: i32,
    pub max_level: i32,
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
            data: Arc::new(Vec::new()),
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
            ir_format: TextureFormat::Rgba8Unorm,
            real_pixels: false,
            gpu_authoritative: false,
            shared: None,
            shared_version: 0,
        }
    }
}

impl GlTexture {
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
#[derive(Debug, Default)]
pub struct Textures {
    map: HashMap<u32, GlTexture>,
    next_name: u32,
    next_generation: u64,
}

impl Textures {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next_name: 1,
            next_generation: 1,
        }
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
        self.map.entry(name).or_default();
        name
    }

    /// Materialize a non-zero name bound through `GL_CHROMIUM_bind_generates_resource`.
    pub fn ensure(&mut self, name: u32) {
        if name != 0 {
            self.map.entry(name).or_default();
            self.next_name = self.next_name.max(name.saturating_add(1));
        }
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
        if source.data.len() != source.w.max(0) as usize * source.h.max(0) as usize * 4 {
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
        if sx + width > source_width
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

    /// `glTexImage2D` — (re)define the texture's RGBA8 pixels + extent + neutral texel `format`, bumping
    /// its generation. `pixels` is the already-converted RGBA8 image (`w*h*4` bytes) or empty for a
    /// storage-only define (e.g. an FBO color attachment allocated before it is rendered into).
    pub fn image_2d(&mut self, name: u32, w: i32, h: i32, pixels: &[u8], format: TextureFormat) {
        // The zeroed-storage byte size, computed in usize with a checked multiply so a large (or
        // adversarial) w*h never overflows an i32 and panics — an out-of-range extent saturates to a
        // no-alloc `None` rather than crashing the driver (the record layer raises GL_INVALID_VALUE).
        let want = (w.max(0) as usize)
            .checked_mul(h.max(0) as usize)
            .and_then(|n| n.checked_mul(4));
        let generation = self.generation();
        let t = self.map.entry(name).or_default();
        t.shared = None;
        t.shared_version = 0;
        t.w = w;
        t.h = h;
        t.ir_format = format;
        if !pixels.is_empty() {
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
        texture.data = Arc::new(Vec::new());
        texture.real_pixels = false;
        texture.gpu_authoritative = false;
        texture.gen = generation;
    }

    /// `glTexStorage2D`/`glTexStorage3D` — allocate immutable RGBA8 storage (`w*h*4` zeroed bytes),
    /// mark the texture immutable, and record the declared `levels`. Mirrors `gl_shim.c`, which allocates
    /// the RGBA8 base plane so a later `glTexSubImage*` has a target (format/levels are not otherwise
    /// materialized). Returns `false` if the `w*h*4` byte size overflows.
    pub fn storage_2d(
        &mut self,
        name: u32,
        w: i32,
        h: i32,
        levels: i32,
        format: TextureFormat,
    ) -> bool {
        let Some(size) = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
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

    /// Allocate (or grow to) a `w*h*4` RGBA8 base plane for a 2D-array / 3D texture upload
    /// (`glTexImage3D`/`glTexStorage3D`) — `gl_shim.c` materializes only the layer-0 plane. Returns
    /// `false` on a size overflow.
    pub fn alloc_rgba(&mut self, name: u32, w: i32, h: i32) -> bool {
        let Some(size) = (w.max(0) as usize)
            .checked_mul(h.max(0) as usize)
            .and_then(|n| n.checked_mul(4))
        else {
            return false;
        };
        let generation = self.generation();
        let t = self.map.entry(name).or_default();
        t.shared = None;
        t.shared_version = 0;
        t.w = w;
        t.h = h;
        if t.data.len() != size {
            t.data = Arc::new(vec![0u8; size]);
        }
        t.real_pixels = false;
        t.gpu_authoritative = false;
        t.gen = generation;
        true
    }

    /// `glTexSubImage2D` / `glCopyTexSubImage2D` — overwrite the `[xo,xo+w) × [yo,yo+h)` sub-rect of an
    /// existing texture's RGBA8 plane with `rgba` (`w*h*4` tightly-packed bytes). Silently clips to the
    /// texture bounds (the caller validates the range first). Returns `false` for an unknown/dataless name.
    pub fn sub_image_2d(
        &mut self,
        name: u32,
        xo: i32,
        yo: i32,
        w: i32,
        h: i32,
        rgba: &[u8],
    ) -> bool {
        let Some(texture) = self.map.get(&name) else {
            return false;
        };
        if texture.data.is_empty() || w <= 0 || h <= 0 || xo < 0 || yo < 0 {
            return false;
        }
        let (tw, th) = (texture.w as usize, texture.h as usize);
        let (xo, yo, w, h) = (xo as usize, yo as usize, w as usize, h as usize);
        let Some(row_bytes) = w.checked_mul(4) else {
            return false;
        };
        let Some(upload_bytes) = row_bytes.checked_mul(h) else {
            return false;
        };
        if xo.checked_add(w).is_none_or(|end| end > tw)
            || yo.checked_add(h).is_none_or(|end| end > th)
            || rgba.len() < upload_bytes
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
                let dst = ((yo + row) * tw + xo) * 4;
                let src = row * row_bytes;
                texture.data[dst..dst + row_bytes] == rgba[src..src + row_bytes]
            });
        if unchanged {
            return true;
        }

        let generation = self.generation();
        let Some(texture) = self.map.get_mut(&name) else {
            return false;
        };
        for row in 0..h {
            let dst = ((yo + row) * tw + xo) * 4;
            let src = row * row_bytes;
            Arc::make_mut(&mut texture.data)[dst..dst + row_bytes]
                .copy_from_slice(&rgba[src..src + row_bytes]);
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
                GL_TEXTURE_BASE_LEVEL => t.base_level = value as i32,
                GL_TEXTURE_MAX_LEVEL => t.max_level = value as i32,
                _ => {}
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
