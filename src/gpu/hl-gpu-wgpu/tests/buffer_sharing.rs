#![cfg(target_os = "macos")]

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::runtime::model::sharing::{ExportId, Exports};
use hl_gpu::{BufferId, Cmd, GpuError, GpuExecutor, SessionResources};
use hl_gpu::{FakeClock, GlobalLedger, Limits, Session};
use hl_gpu_wgpu::{Device, DeviceConfig};

#[test]
fn two_sessions_alias_one_metal_texture_without_a_copy() {
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
    use hl_gpu::{CommandBuffer, Enc, TextureId};
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_exec = device.executor();
    let mut importer_exec = device.executor();
    let exports = Exports::new();
    let global = GlobalLedger::unbounded();
    let mut owner = session(&owner_exec, exports.clone(), global.clone());
    let mut importer = session(&importer_exec, exports, global);
    let desc = TextureDesc { width: 2, height: 1, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm, usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::SAMPLED, label: String::new() };
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[
        Cmd::CreateTexture(1, desc),
        Cmd::Submit(CommandBuffer { encoder: vec![Enc::ClearRect { texture: 1, x: 0, y: 0, w: 2, h: 1, color: [1.0, 0.0, 0.0, 1.0], base_array_layer: 0, layer_count: 1, mip_level: 0 }], signal: None }),
    ]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_texture(&mut owner, &owner_exec, TextureId(1)).unwrap();
    assert_eq!(hl_gpu::runtime::service::dispatch::import_texture(&mut importer, &importer_exec, TextureId(9), export).unwrap(), 8);
    assert_eq!(importer_exec.read_texture(&importer.resources, 9).unwrap(), [255, 0, 0, 255, 255, 0, 0, 255]);
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::DestroyTexture(1)]).unwrap();
    assert_eq!(importer_exec.read_texture(&importer.resources, 9).unwrap(), [255, 0, 0, 255, 255, 0, 0, 255]);
}

#[test]
fn two_sessions_alias_one_metal_buffer_at_its_authoritative_size() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_executor = device.executor();
    let mut importer_executor = device.executor();
    let mut owner = SessionResources::new();
    let mut importer = SessionResources::new();
    let size = 13;

    owner_executor
        .execute(
            &mut owner,
            &[
                Cmd::CreateBuffer(
                    1,
                    BufferDesc {
                        size,
                        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                        label: String::new(),
                    },
                ),
                Cmd::WriteBuffer {
                    id: 1,
                    offset: 0,
                    data: b"owner-visible".to_vec(),
                },
            ],
        )
        .expect("create owner buffer");

    let (shared, authoritative_size) = owner_executor
        .export_buffer(&owner, BufferId(1))
        .expect("export owner buffer");
    assert_eq!(authoritative_size, size);

    let wrong_size = importer_executor
        .import_buffer(shared.clone(), authoritative_size + 1)
        .expect_err("an importer cannot widen the shared allocation");
    assert!(matches!(wrong_size, GpuError::Invalid(_)));

    let native = importer_executor
        .import_buffer(shared, authoritative_size)
        .expect("import exact authoritative size");
    importer.buffers.insert(9, native).expect("unused local id");

    assert_eq!(
        importer_executor
            .read_buffer(&importer, BufferId(9), 0, size as usize)
            .expect("read owner write through importer alias"),
        b"owner-visible"
    );

    importer_executor
        .execute(
            &mut importer,
            &[Cmd::WriteBuffer {
                id: 9,
                offset: 0,
                data: b"alias-visible".to_vec(),
            }],
        )
        .expect("write through importer alias");
    assert_eq!(
        owner_executor
            .read_buffer(&owner, BufferId(1), 0, size as usize)
            .expect("read importer write through owner alias"),
        b"alias-visible"
    );
}

#[test]
fn an_independently_acquired_device_refuses_the_alias() {
    let owner_device = Device::new(DeviceConfig::default()).expect("owner Metal adapter");
    let importer_device = Device::new(DeviceConfig::default()).expect("importer Metal adapter");
    let mut owner_executor = owner_device.executor();
    let importer_executor = importer_device.executor();
    let mut owner = SessionResources::new();

    owner_executor
        .execute(
            &mut owner,
            &[Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            )],
        )
        .expect("create owner buffer");
    let (shared, bytes) = owner_executor
        .export_buffer(&owner, BufferId(1))
        .expect("export owner buffer");

    let error = match importer_executor.import_buffer(shared, bytes) {
        Ok(_) => panic!("cross-device alias must be refused before insertion"),
        Err(error) => error,
    };
    assert!(matches!(error, GpuError::Invalid(_)));
}

fn session(executor: &impl GpuExecutor, exports: Exports, global: GlobalLedger) -> Session {
    Session::new(
        Limits::from_capabilities(executor.capabilities()),
        global,
        Box::new(FakeClock::new(0)),
    )
    .with_exports(exports)
}

#[test]
fn runtime_sharing_lifecycle_is_atomic_and_retains_live_imports() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_exec = device.executor();
    let mut importer_exec = device.executor();
    let exports = Exports::new();
    let global = GlobalLedger::unbounded();
    let mut owner = session(&owner_exec, exports.clone(), global.clone());
    let mut importer = session(&importer_exec, exports.clone(), global);
    let desc = BufferDesc {
        size: 4,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut owner,
        &mut owner_exec,
        0,
        &[
            Cmd::CreateBuffer(1, desc),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![1, 2, 3, 4],
            },
        ],
    )
    .unwrap();
    let export =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &owner_exec, BufferId(1))
            .unwrap();
    assert!(
        hl_gpu::runtime::service::dispatch::import_buffer(
            &mut importer,
            &importer_exec,
            BufferId(7),
            ExportId(u64::MAX)
        )
        .is_err()
    );
    assert_eq!(
        hl_gpu::runtime::service::dispatch::import_buffer(
            &mut importer,
            &importer_exec,
            BufferId(7),
            export
        )
        .unwrap(),
        4
    );
    assert!(
        hl_gpu::runtime::service::dispatch::import_buffer(
            &mut importer,
            &importer_exec,
            BufferId(7),
            export
        )
        .is_err()
    );
    assert_eq!(
        importer_exec
            .read_buffer(&importer.resources, BufferId(7), 0, 4)
            .unwrap(),
        [1, 2, 3, 4]
    );
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::DestroyBuffer(1)]).unwrap();
    assert!(exports.is_live(export));
    assert_eq!(
        importer_exec
            .read_buffer(&importer.resources, BufferId(7), 0, 4)
            .unwrap(),
        [1, 2, 3, 4]
    );
    hl_gpu::runtime::submit(
        &mut importer,
        &mut importer_exec,
        0,
        &[Cmd::DestroyBuffer(7)],
    )
    .unwrap();
    assert!(!exports.is_live(export));
}

#[test]
fn shared_destroy_then_create_same_id_keeps_replacement_legal() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut exec = device.executor();
    let exports = Exports::new();
    let mut owner = session(&exec, exports.clone(), GlobalLedger::unbounded());
    let desc = BufferDesc {
        size: 4,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut owner,
        &mut exec,
        0,
        &[Cmd::CreateBuffer(1, desc.clone())],
    )
    .unwrap();
    let old =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &exec, BufferId(1)).unwrap();
    hl_gpu::runtime::submit(
        &mut owner,
        &mut exec,
        0,
        &[
            Cmd::DestroyBuffer(1),
            Cmd::CreateBuffer(1, desc),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![4, 3, 2, 1],
            },
        ],
    )
    .unwrap();
    assert!(!exports.is_live(old));
    assert_eq!(
        exec.read_buffer(&owner.resources, BufferId(1), 0, 4)
            .unwrap(),
        [4, 3, 2, 1]
    );
    assert_eq!(owner.account.ledger().totals.bytes, 4);
}

#[test]
fn repeated_destroy_recreate_of_a_shared_id_tracks_each_sequential_lifetime() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut exec = device.executor();
    let exports = Exports::new();
    let mut owner = session(&exec, exports.clone(), GlobalLedger::unbounded());
    let desc = BufferDesc {
        size: 4,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut owner,
        &mut exec,
        0,
        &[Cmd::CreateBuffer(1, desc.clone())],
    )
    .unwrap();
    let old =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &exec, BufferId(1)).unwrap();
    hl_gpu::runtime::submit(
        &mut owner,
        &mut exec,
        0,
        &[
            Cmd::DestroyBuffer(1),
            Cmd::CreateBuffer(1, desc.clone()),
            Cmd::DestroyBuffer(1),
            Cmd::CreateBuffer(1, desc),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![7, 6, 5, 4],
            },
        ],
    )
    .unwrap();
    assert!(!exports.is_live(old));
    assert_eq!(
        exec.read_buffer(&owner.resources, BufferId(1), 0, 4)
            .unwrap(),
        [7, 6, 5, 4]
    );
    let ledger = owner.account.ledger();
    assert_eq!(
        ledger.live.get(&(hl_gpu::runtime::KIND_BUFFER, 1)),
        Some(&4)
    );
    assert_eq!(ledger.totals.bytes, 4);
}

#[test]
fn active_payer_importer_destroy_recreate_preserves_the_replacement_charge() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_exec = device.executor();
    let mut importer_exec = device.executor();
    let exports = Exports::new();
    let global = GlobalLedger::new(1024, 16);
    let make = |exec: &hl_gpu_wgpu::WgpuExecutor| {
        Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone())
    };
    let mut owner = make(&owner_exec);
    let mut importer = make(&importer_exec);
    let old_desc = BufferDesc {
        size: 4,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    let new_desc = BufferDesc {
        size: 8,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut owner,
        &mut owner_exec,
        0,
        &[Cmd::CreateBuffer(1, old_desc)],
    )
    .unwrap();
    let export =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &owner_exec, BufferId(1))
            .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(
        &mut importer,
        &importer_exec,
        BufferId(2),
        export,
    )
    .unwrap();
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::DestroyBuffer(1)]).unwrap();
    assert_eq!(importer.account.reserved_bytes(), 0);
    hl_gpu::runtime::submit(
        &mut importer,
        &mut importer_exec,
        0,
        &[
            Cmd::DestroyBuffer(2),
            Cmd::CreateBuffer(2, new_desc),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
        ],
    )
    .unwrap();
    assert!(!exports.is_live(export));
    assert_eq!(
        importer_exec
            .read_buffer(&importer.resources, BufferId(2), 0, 8)
            .unwrap(),
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
    let ledger = importer.account.ledger();
    assert_eq!(
        ledger.live.get(&(hl_gpu::runtime::KIND_BUFFER, 2)),
        Some(&8)
    );
    assert_eq!(ledger.totals.bytes, 8);
    assert_eq!(global.residency_bytes(), 8);
}

#[test]
fn payer_moves_to_lowest_remaining_importer_and_disconnect_returns_to_baseline() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_exec = device.executor();
    let first_exec = device.executor();
    let second_exec = device.executor();
    let exports = Exports::new();
    let global = GlobalLedger::new(1024, 16);
    let make = |exec: &hl_gpu_wgpu::WgpuExecutor| {
        Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone())
    };
    let mut owner = make(&owner_exec);
    let mut first = make(&first_exec);
    let mut second = make(&second_exec);
    let desc = BufferDesc {
        size: 4,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut owner,
        &mut owner_exec,
        0,
        &[
            Cmd::CreateBuffer(1, desc),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![9, 8, 7, 6],
            },
        ],
    )
    .unwrap();
    let export =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &owner_exec, BufferId(1))
            .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(&mut first, &first_exec, BufferId(2), export)
        .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(
        &mut second,
        &second_exec,
        BufferId(3),
        export,
    )
    .unwrap();
    assert_eq!(first.account.reserved_bytes(), 4);
    assert_eq!(second.account.reserved_bytes(), 4);
    assert_eq!(global.residency_bytes(), 4);
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::DestroyBuffer(1)]).unwrap();
    assert_eq!(global.residency_bytes(), 4);
    assert_eq!(
        first.account.reserved_bytes(),
        0,
        "lowest importer became the active payer"
    );
    assert_eq!(
        second.account.reserved_bytes(),
        4,
        "nonpayer retains only its bounded reservation"
    );
    drop(first);
    assert_eq!(global.residency_bytes(), 4);
    assert_eq!(
        second.account.reserved_bytes(),
        0,
        "active payer disconnect transfers and consumes reservation"
    );
    assert_eq!(
        second_exec
            .read_buffer(&second.resources, BufferId(3), 0, 4)
            .unwrap(),
        [9, 8, 7, 6]
    );
    second.release_all();
    assert_eq!(global.residency_bytes(), 0);
    assert!(!exports.is_live(export));
}

#[path = "buffer_sharing/lifecycle.rs"]
mod lifecycle;
