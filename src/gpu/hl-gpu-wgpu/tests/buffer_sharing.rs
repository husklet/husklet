#![cfg(target_os = "macos")]

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{BufferId, Cmd, GpuError, GpuExecutor, SessionResources};
use hl_gpu_wgpu::{Device, DeviceConfig};

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
