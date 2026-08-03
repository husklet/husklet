pub(super) fn inject(module: &mut naga::Module, layouts: &[crate::reflect::SamplerMetadataLayout]) {
    if layouts.is_empty() {
        return;
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
}
