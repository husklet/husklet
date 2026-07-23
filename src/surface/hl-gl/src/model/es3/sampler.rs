use super::*;

// Sampler objects (glGenSamplers / glBindSampler / glSamplerParameter*)
// ==================================================================================================

/// One ES3 sampler object's full parameter state (ES 3.0 §6.10 default table). The min/max LOD are the
/// only non-enum, float-typed parameters; everything else is a GL enum stored as its `i32` value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SamplerObj {
    pub min_filter: i32,
    pub mag_filter: i32,
    pub wrap_s: i32,
    pub wrap_t: i32,
    pub wrap_r: i32,
    pub min_lod: f32,
    pub max_lod: f32,
    pub compare_mode: i32,
    pub compare_func: i32,
}

impl Default for SamplerObj {
    fn default() -> Self {
        SamplerObj {
            min_filter: GL_NEAREST_MIPMAP_LINEAR as i32,
            mag_filter: GL_LINEAR as i32,
            wrap_s: GL_REPEAT as i32,
            wrap_t: GL_REPEAT as i32,
            wrap_r: GL_REPEAT as i32,
            min_lod: -1000.0,
            max_lod: 1000.0,
            compare_mode: GL_NONE as i32,
            compare_func: GL_LEQUAL as i32,
        }
    }
}

impl SamplerObj {
    pub fn accepts(pname: u32, value: i32) -> bool {
        match pname {
            GL_TEXTURE_MIN_FILTER => matches!(
                value as u32,
                GL_NEAREST
                    | GL_LINEAR
                    | GL_NEAREST_MIPMAP_NEAREST
                    | GL_LINEAR_MIPMAP_NEAREST
                    | GL_NEAREST_MIPMAP_LINEAR
                    | GL_LINEAR_MIPMAP_LINEAR
            ),
            GL_TEXTURE_MAG_FILTER => matches!(value as u32, GL_NEAREST | GL_LINEAR),
            GL_TEXTURE_WRAP_S | GL_TEXTURE_WRAP_T | GL_TEXTURE_WRAP_R => matches!(
                value as u32,
                GL_REPEAT | GL_CLAMP_TO_EDGE | GL_MIRRORED_REPEAT
            ),
            GL_TEXTURE_COMPARE_MODE => matches!(value as u32, GL_NONE | GL_COMPARE_REF_TO_TEXTURE),
            GL_TEXTURE_COMPARE_FUNC => matches!(
                value as u32,
                GL_NEVER
                    | GL_LESS
                    | GL_EQUAL
                    | GL_LEQUAL
                    | GL_GREATER
                    | GL_NOTEQUAL
                    | GL_GEQUAL
                    | GL_ALWAYS
            ),
            GL_TEXTURE_MIN_LOD | GL_TEXTURE_MAX_LOD => true,
            _ => false,
        }
    }

    /// The neutral min-filter for this sampler object's GL min-filter (Linear for `LINEAR` /
    /// `LINEAR_MIPMAP_*`, else Nearest) — the exact mapping [`super::texture::GlTexture::ir_min_filter`]
    /// uses, so a bound sampler object lowers identically to the equivalent texture parameters.
    pub fn ir_min_filter(&self) -> Filter {
        match self.min_filter as u32 {
            GL_LINEAR | GL_LINEAR_MIPMAP_NEAREST | GL_LINEAR_MIPMAP_LINEAR => Filter::Linear,
            _ => Filter::Nearest,
        }
    }

    /// The neutral mag-filter (Linear only for exactly `LINEAR`).
    pub fn ir_mag_filter(&self) -> Filter {
        if self.mag_filter as u32 == GL_LINEAR {
            Filter::Linear
        } else {
            Filter::Nearest
        }
    }

    /// The neutral S wrap (ClampToEdge / MirrorRepeat / else Repeat).
    pub fn ir_wrap_s(&self) -> AddressMode {
        Self::address_mode(self.wrap_s as u32)
    }

    /// The neutral T wrap.
    pub fn ir_wrap_t(&self) -> AddressMode {
        Self::address_mode(self.wrap_t as u32)
    }

    /// Read one parameter as `f32` (the int-typed getter rounds this to nearest). `None` for an unknown
    /// `pname` (the caller raises `GL_INVALID_ENUM`).
    pub fn get(&self, pname: u32) -> Option<f32> {
        Some(match pname {
            GL_TEXTURE_MIN_FILTER => self.min_filter as f32,
            GL_TEXTURE_MAG_FILTER => self.mag_filter as f32,
            GL_TEXTURE_WRAP_S => self.wrap_s as f32,
            GL_TEXTURE_WRAP_T => self.wrap_t as f32,
            GL_TEXTURE_WRAP_R => self.wrap_r as f32,
            GL_TEXTURE_COMPARE_MODE => self.compare_mode as f32,
            GL_TEXTURE_COMPARE_FUNC => self.compare_func as f32,
            GL_TEXTURE_MIN_LOD => self.min_lod,
            GL_TEXTURE_MAX_LOD => self.max_lod,
            _ => return None,
        })
    }
}

/// GL wrap enum → neutral address mode (ClampToEdge / MirrorRepeat / else Repeat), matching the texture
/// path's `address_mode`.
impl SamplerObj {
    fn address_mode(gl: u32) -> AddressMode {
        match gl {
            GL_CLAMP_TO_EDGE => AddressMode::ClampToEdge,
            GL_MIRRORED_REPEAT => AddressMode::MirrorRepeat,
            _ => AddressMode::Repeat,
        }
    }
}

/// The per-context sampler-object table: reserved names (`glGenSamplers`), instantiated objects (created
/// lazily on first parameterize/bind), and the per-unit binding map. Name `0` is the reserved sentinel.
#[derive(Debug, Default)]
pub struct Samplers {
    reserved: HashSet<u32>,
    objects: HashMap<u32, SamplerObj>,
    binding: HashMap<u32, u32>,
    next_name: u32,
}

impl Samplers {
    pub fn new() -> Self {
        Self {
            reserved: HashSet::new(),
            objects: HashMap::new(),
            binding: HashMap::new(),
            next_name: 1,
        }
    }

    /// `glGenSamplers` — mint one fresh reserved name.
    pub fn gen(&mut self) -> u32 {
        let id = self.next_name;
        self.next_name += 1;
        self.reserved.insert(id);
        id
    }

    /// A name is usable iff `glGenSamplers` handed it out and it is not deleted (reserved OR live).
    pub fn known(&self, id: u32) -> bool {
        id != 0 && (self.objects.contains_key(&id) || self.reserved.contains(&id))
    }

    /// `glIsSampler` — true only once the name names a CREATED object (bound/parameterized), not merely
    /// reserved (the lazy-instantiation model GL's buffer/texture names use).
    pub fn contains(&self, id: u32) -> bool {
        self.objects.contains_key(&id)
    }

    /// Instantiate (if needed) and mutably borrow the object, moving it out of the reserved set.
    pub fn instantiate(&mut self, id: u32) -> &mut SamplerObj {
        self.reserved.remove(&id);
        self.objects.entry(id).or_default()
    }

    pub fn get(&self, id: u32) -> Option<&SamplerObj> {
        self.objects.get(&id)
    }

    /// `glBindSampler(unit, id)` — bind `id` to `unit` (`0` clears the binding).
    pub fn bind(&mut self, unit: u32, id: u32) {
        if id == 0 {
            self.binding.remove(&unit);
        } else {
            self.binding.insert(unit, id);
        }
    }

    pub fn binding(&self, unit: u32) -> u32 {
        self.binding.get(&unit).copied().unwrap_or(0)
    }

    /// `glDeleteSamplers` (one name) — drop the object + reservation and unbind it from every unit.
    pub fn delete(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        self.objects.remove(&id);
        self.reserved.remove(&id);
        self.binding.retain(|_, v| *v != id);
    }
}
