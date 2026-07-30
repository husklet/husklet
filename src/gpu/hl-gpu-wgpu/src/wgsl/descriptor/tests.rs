use super::*;
use hl_gpu::protocol::model::descriptor::{PipelineBinding, PipelineBindingKind};

#[test]
fn dynamic_uniform_read_becomes_scalar_bindings_and_select() {
    let mut module = naga::front::wgsl::parse_str(
        r#"
            struct Item { value: vec4<u32> }
            @group(0) @binding(0) var<uniform> items: binding_array<Item, 2>;
            @group(0) @binding(1) var<uniform> selected: vec4<u32>;
            @compute @workgroup_size(1)
            fn main() {
                let item = items[selected.x];
                let value = item.value.x;
            }
        "#,
    )
    .unwrap();
    let layout = PipelineLayout {
        bindings: vec![
            PipelineBinding {
                group: 0,
                binding: 0,
                count: 2,
                kind: PipelineBindingKind::UniformBuffer,
            },
            PipelineBinding {
                group: 0,
                binding: 1,
                count: 1,
                kind: PipelineBindingKind::UniformBuffer,
            },
        ],
    };

    ScalarArrays::lower(&mut module, &layout).unwrap();
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    let source =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .unwrap();

    assert!(!source.contains("binding_array"));
    assert!(source.contains("@binding(0)"));
    assert!(source.contains("@binding(2)"));
    assert!(source.contains("select("));
}

#[test]
fn dynamic_storage_write_and_atomic_use_bounded_scalar_switches() {
    let mut module = naga::front::wgsl::parse_str(
        r#"
            struct Item {
                value: u32,
                counter: atomic<u32>,
            }
            @group(0) @binding(0) var<storage, read_write> items: binding_array<Item, 2>;
            @group(0) @binding(1) var<uniform> selected: vec4<u32>;
            @compute @workgroup_size(1)
            fn main() {
                if selected.y == 0u {
                    items[selected.x].value = 41u;
                }
                var previous = 0u;
                loop {
                    previous = atomicAdd(&items[selected.x].counter, 1u);
                    break;
                }
                items[0].value = previous;
            }
        "#,
    )
    .unwrap();
    let layout = PipelineLayout {
        bindings: vec![
            PipelineBinding {
                group: 0,
                binding: 0,
                count: 2,
                kind: PipelineBindingKind::StorageBuffer,
            },
            PipelineBinding {
                group: 0,
                binding: 1,
                count: 1,
                kind: PipelineBindingKind::UniformBuffer,
            },
        ],
    };

    ScalarArrays::lower(&mut module, &layout).unwrap();
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    let source =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .unwrap();

    assert!(!source.contains("binding_array"));
    assert!(source.contains("@binding(0)"));
    assert!(source.contains("@binding(2)"));
    assert_eq!(source.matches("switch").count(), 2);
    assert_eq!(source.matches("default").count(), 2);
}

#[test]
fn dynamic_storage_image_load_store_and_query_are_bounded() {
    let mut module = naga::front::wgsl::parse_str(
        r#"
            @group(0) @binding(0)
            var images: binding_array<texture_storage_2d<r32uint, read_write>, 2>;
            @group(0) @binding(1) var<uniform> selected: vec4<u32>;
            @group(0) @binding(2) var<storage, read_write> output: array<u32>;
            @group(0) @binding(3)
            var atomic_images: binding_array<texture_storage_2d<r32uint, atomic>, 2>;
            @compute @workgroup_size(1)
            fn main() {
                let size = textureDimensions(images[selected.x]);
                let value = textureLoad(images[selected.x], vec2<i32>(0, 0));
                textureStore(images[selected.x], vec2<i32>(0, 0), vec4<u32>(7, 0, 0, 0));
                textureAtomicAdd(atomic_images[selected.x], vec2<i32>(0, 0), 1u);
                output[0] = size.x + value.x;
            }
        "#,
    )
    .unwrap();
    let layout = PipelineLayout {
        bindings: vec![
            PipelineBinding {
                group: 0,
                binding: 0,
                count: 2,
                kind: PipelineBindingKind::StorageTexture,
            },
            PipelineBinding {
                group: 0,
                binding: 1,
                count: 1,
                kind: PipelineBindingKind::UniformBuffer,
            },
            PipelineBinding {
                group: 0,
                binding: 2,
                count: 1,
                kind: PipelineBindingKind::StorageBuffer,
            },
            PipelineBinding {
                group: 0,
                binding: 3,
                count: 2,
                kind: PipelineBindingKind::StorageTexture,
            },
        ],
    };

    ScalarArrays::lower(&mut module, &layout).unwrap();
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    let source =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .unwrap();

    assert!(!source.contains("binding_array"));
    assert!(source.contains("@binding(0)"));
    assert!(source.contains("@binding(3)"));
    assert!(source.contains("switch"));
    assert!(source.contains("default"));
    assert!(source.matches("select(").count() >= 2);
}
