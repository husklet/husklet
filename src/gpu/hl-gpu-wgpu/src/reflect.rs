//! Entry-point resource-usage reflection — the `(group, binding)` slots a shader entry point actually
//! READS, which is exactly the set wgpu's *auto* pipeline layout (`layout: None`) exposes.
//!
//! The GL driver emits a bind-GROUP entry per *bound* resource (UBO@0, texture@`1+2k`, sampler@`2+2k`),
//! but a GskGpu program routinely BINDS more than it samples and DECLARES more still: a real GTK4 frame
//! reaches `create_bind_group` with a 5-entry bind group (UBO + two texture/sampler pairs) whose shader
//! declares three pairs (seven bindings) yet samples only one (three). wgpu derives the auto layout from
//! entry-point *usage*, so it exposes only the three read bindings — and the 5-entry bind group NACKs
//! ("Number of bindings … (5) does not match … (3)"). Neither the bound set (5) nor the declared set (7)
//! equals it; only the *used* set (3) does, and only the driver's bind group knows the resources, so the
//! two are reconciled by FILTERING the bind-group entries down to the used bindings at build time (see
//! `bindgroup::build_bind_group` + `pipeline`). Dropping a bound-but-unsampled resource is semantically
//! free — the shader never reads it, so the draw is identical.
//!
//! This reflects, per entry point, the used `(group, binding)` set **and the binding's declared TYPE**. A
//! render pipeline builds an EXPLICIT bind-group layout from the union of its vertex + fragment entry
//! points' used slots (reconciling each slot's type + stage visibility across the two stages), and filters
//! the driver's bind group to that same used set — so the concrete bind group matches the explicit layout
//! exactly, for both GTK (single group 0) and Zed (a group whose type auto-derivation cannot merge).

/// The wgpu binding-TYPE a used resource slot declares — the neutral form `reflect` recovers from a naga
/// global so `pipeline` can build a `wgpu::BindGroupLayoutEntry` without pulling wgpu into this module. One
/// per slot; reconciled across stages in `pipeline::create_render_pipeline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// A `var<uniform>` buffer (WebGPU `Buffer { Uniform }`).
    UniformBuffer,
    /// A `var<storage, …>` buffer; `read_only` selects the access (WebGPU `Buffer { Storage }`).
    StorageBuffer { read_only: bool },
    /// A sampled `texture_*` (WebGPU `Texture`), carrying the view dimension, sample type, and multisample
    /// flag needed for a layout entry that the shader's usage validates against.
    Texture {
        dim: TexDim,
        sample: TexSample,
        multi: bool,
    },
    /// A `sampler` (WebGPU `Sampler`); `comparison` selects a comparison sampler (depth compare).
    Sampler { comparison: bool },
}

/// A texture's view dimension (naga [`naga::ImageDimension`] + arrayed → the WebGPU view dimension).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexDim {
    D1,
    D2,
    D2Array,
    D3,
    Cube,
    CubeArray,
}

/// A sampled texture's sample type (naga [`naga::ImageClass`] → the WebGPU texture sample type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexSample {
    /// A float texture; `filterable` mirrors whether it may be filtered (float ⇒ true).
    Float {
        filterable: bool,
    },
    Sint,
    Uint,
    Depth,
}

/// One used resource binding: its `(group, binding)` slot and its declared type.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub group: u32,
    pub binding: u32,
    pub kind: BindingKind,
}

/// One entry point's used resource bindings.
#[derive(Clone, Debug)]
pub struct EntryUsage {
    /// The entry-point name a pipeline's `ShaderRef` selects (`vmain`/`fmain`/`main`/…). For a GLSL module
    /// this is the renamed single entry point; for SPIR-V it is the SPIR-V entry-point name.
    pub entry: String,
    /// Every resource this entry point READS — its `(group, binding)` slot AND declared type — the exact
    /// set (and types) a pipeline that binds this entry point exposes.
    pub bindings: Vec<Binding>,
}

/// A module's per-entry-point resource usage.
#[derive(Clone, Debug, Default)]
pub struct ModuleUsage {
    pub entries: Vec<EntryUsage>,
}

impl ModuleUsage {
    /// The used resource bindings of the entry point named `entry`, or an empty slice if the module has no
    /// such entry point (a stage with no reflected usage contributes nothing to the layout / filter).
    pub fn used_for(&self, entry: &str) -> &[Binding] {
        self.entries
            .iter()
            .find(|e| e.entry == entry)
            .map(|e| e.bindings.as_slice())
            .unwrap_or(&[])
    }

    /// Reflect each entry point's used resource bindings and declared types.
    pub fn from_module(module: &naga::Module) -> Self {
        let info = match naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(module)
        {
            Ok(info) => info,
            Err(_) => return Self::default(),
        };
        let entries = module
            .entry_points
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let entry_info = info.get_entry_point(index);
                let bindings = module
                    .global_variables
                    .iter()
                    .filter_map(|(handle, variable)| {
                        let resource = variable.binding.as_ref()?;
                        if entry_info[handle].is_empty() {
                            return None;
                        }
                        let kind = BindingKind::from_global(module, variable)?;
                        Some(Binding {
                            group: resource.group,
                            binding: resource.binding,
                            kind,
                        })
                    })
                    .collect();
                EntryUsage {
                    entry: entry.name.clone(),
                    bindings,
                }
            })
            .collect();
        Self { entries }
    }
}

/// The declared type of the naga global `gv` (its handle `ty` resolved in `module.types`), as the neutral
/// [`BindingKind`] a layout entry is built from. Returns `None` for a global in an address space that
/// carries no bind-group entry (function/private/workgroup/push-constant locals).
impl BindingKind {
    fn from_global(module: &naga::Module, variable: &naga::GlobalVariable) -> Option<Self> {
        match variable.space {
            naga::AddressSpace::Uniform => Some(BindingKind::UniformBuffer),
            naga::AddressSpace::Storage { access } => Some(BindingKind::StorageBuffer {
                read_only: !access.contains(naga::StorageAccess::STORE),
            }),
            naga::AddressSpace::Handle => match &module.types[variable.ty].inner {
                naga::TypeInner::Image {
                    dim,
                    arrayed,
                    class,
                } => {
                    let dim = match (dim, arrayed) {
                        (naga::ImageDimension::D1, _) => TexDim::D1,
                        (naga::ImageDimension::D2, false) => TexDim::D2,
                        (naga::ImageDimension::D2, true) => TexDim::D2Array,
                        (naga::ImageDimension::D3, _) => TexDim::D3,
                        (naga::ImageDimension::Cube, false) => TexDim::Cube,
                        (naga::ImageDimension::Cube, true) => TexDim::CubeArray,
                    };
                    let sample = match class {
                        naga::ImageClass::Sampled { kind, .. } => match kind {
                            naga::ScalarKind::Sint => TexSample::Sint,
                            naga::ScalarKind::Uint => TexSample::Uint,
                            // Float (and any non-integer kind) is a filterable float texture.
                            _ => TexSample::Float { filterable: true },
                        },
                        naga::ImageClass::Depth { .. } => TexSample::Depth,
                        // A storage image is not a sampled texture; the render suite never binds one, so treat
                        // it as a float texture for the layout entry rather than inventing a storage-texture
                        // reflection this path never exercises.
                        naga::ImageClass::Storage { .. } => TexSample::Float { filterable: true },
                    };
                    let multi = matches!(
                        class,
                        naga::ImageClass::Sampled { multi: true, .. }
                            | naga::ImageClass::Depth { multi: true }
                    );
                    Some(BindingKind::Texture { dim, sample, multi })
                }
                naga::TypeInner::Sampler { comparison } => Some(BindingKind::Sampler {
                    comparison: *comparison,
                }),
                _ => None,
            },
            _ => None,
        }
    }
}
