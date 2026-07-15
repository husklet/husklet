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
//! This reflects, per entry point, the used `(group, binding)` set. A pipeline stores the union of its
//! vertex + fragment entry points' sets (its auto layout's exact bindings) and filters against it.

/// One entry point's used resource bindings.
#[derive(Clone, Debug)]
pub struct EntryUsage {
    /// The entry-point name a pipeline's `ShaderRef` selects (`vmain`/`fmain`/`main`/…). For a GLSL module
    /// this is the renamed single entry point; for SPIR-V it is the SPIR-V entry-point name.
    pub entry: String,
    /// The `(group, binding)` of every resource this entry point READS — the auto layout's exact set for
    /// a pipeline that binds this entry point.
    pub bindings: Vec<(u32, u32)>,
}

/// A module's per-entry-point resource usage.
#[derive(Clone, Debug, Default)]
pub struct Reflected {
    pub entries: Vec<EntryUsage>,
}

impl Reflected {
    /// The used `(group, binding)` bindings of the entry point named `entry`, or an empty slice if the
    /// module has no such entry point (a stage with no reflected usage filters nothing).
    pub fn used_for(&self, entry: &str) -> &[(u32, u32)] {
        self.entries
            .iter()
            .find(|e| e.entry == entry)
            .map(|e| e.bindings.as_slice())
            .unwrap_or(&[])
    }
}

/// Reflect each entry point's used resource bindings (see the module docs). Validation mirrors
/// `module_to_wgsl`; on the rare validation failure this returns no usage (the subsequent `module_to_wgsl`
/// surfaces the real error and shader creation aborts), so the filter simply keeps every entry — no worse
/// than the pre-fix behaviour.
pub fn reflect(module: &naga::Module) -> Reflected {
    let info = match naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    {
        Ok(info) => info,
        Err(_) => return Reflected::default(),
    };
    let entries = module
        .entry_points
        .iter()
        .enumerate()
        .map(|(i, ep)| {
            let ep_info = info.get_entry_point(i);
            let bindings = module
                .global_variables
                .iter()
                .filter_map(|(h, gv)| {
                    let rb = gv.binding.as_ref()?;
                    // A global the entry point neither reads nor writes carries an empty use-flag set —
                    // exactly the bindings wgpu prunes from the auto layout.
                    (!ep_info[h].is_empty()).then_some((rb.group, rb.binding))
                })
                .collect();
            EntryUsage { entry: ep.name.clone(), bindings }
        })
        .collect();
    Reflected { entries }
}
