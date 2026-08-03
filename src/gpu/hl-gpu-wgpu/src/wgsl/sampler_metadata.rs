pub(super) fn inject(module: &mut naga::Module, layouts: &[crate::reflect::SamplerMetadataLayout]) {
    if layouts.is_empty() {
        return;
    }
    for (_, variable) in module.global_variables.iter_mut() {
        let Some(binding) = variable.binding.as_ref() else { continue };
        let mut ty = variable.ty;
        if let naga::TypeInner::BindingArray { base, .. } = module.types[ty].inner {
            ty = base;
        }
        if matches!(module.types[ty].inner, naga::TypeInner::Sampler { comparison: false }) {
            variable.name = Some(format!("_hl_sampler_g{}_b{}", binding.group, binding.binding));
        }
    }
    let scalar = module.types.insert(
        naga::Type {
            name: None,
            inner: naga::TypeInner::Scalar(naga::Scalar {
                kind: naga::ScalarKind::Uint,
                width: 4,
            }),
        },
        naga::Span::default(),
    );
    let words = module.types.insert(
        naga::Type {
            name: Some("HlSamplerMetadataWords".into()),
            inner: naga::TypeInner::Array {
                base: scalar,
                size: naga::ArraySize::Dynamic,
                stride: 4,
            },
        },
        naga::Span::default(),
    );
    for layout in layouts {
        module.global_variables.append(
            naga::GlobalVariable {
                name: Some(format!("_hl_sampler_metadata_g{}", layout.group)),
                space: naga::AddressSpace::Storage {
                    access: naga::StorageAccess::LOAD,
                },
                binding: Some(naga::ResourceBinding {
                    group: layout.group,
                    binding: layout.binding,
                }),
                ty: words,
                init: None,
            },
            naga::Span::default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflect::{SamplerMetadataLayout, SamplerMetadataSlot};

    #[test]
    fn injected_binding_is_valid_and_uses_pipeline_allocation() {
        let mut module = naga::Module::default();
        inject(&mut module, &[SamplerMetadataLayout {
            group: 3,
            binding: 27,
            samplers: vec![SamplerMetadataSlot { binding: 5, base_ordinal: 0, count: 1 }],
        }]);
        let variable = module.global_variables.iter().next().unwrap().1;
        assert_eq!(variable.binding, Some(naga::ResourceBinding { group: 3, binding: 27 }));
        assert!(matches!(variable.space, naga::AddressSpace::Storage { access } if access == naga::StorageAccess::LOAD));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("metadata storage declaration must validate");
    }

    #[test]
    fn wgsl_helpers_may_accept_texture_and_sampler_handles() {
        let source = r#"
            fn sample(t: texture_2d<f32>, s: sampler, uv: vec2<f32>) -> vec4<f32> {
                return textureSampleLevel(t, s, uv, 0.0);
            }
            @group(0) @binding(0) var t: texture_2d<f32>;
            @group(0) @binding(1) var s: sampler;
            @fragment fn main() -> @location(0) vec4<f32> { return sample(t, s, vec2(0.5)); }
        "#;
        let module = naga::front::wgsl::parse_str(source).expect("handle parameters parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("handle parameters validate");
    }
}
