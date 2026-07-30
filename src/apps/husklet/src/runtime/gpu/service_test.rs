use super::*;

#[test]
fn service_publishes_and_removes_a_ready_endpoint() {
    let path = std::env::temp_dir().join(format!("hl-gpu-service-{}.sock", std::process::id()));
    let service = Service::start(
        &path,
        Configuration {
            backend: Backend::Cpu,
            trace: false,
        },
    )
    .unwrap();
    assert_eq!(service.socket(), path);
    assert!(path.exists());

    let mut sink = hl_gpu::RemoteCommandSink::new(path.to_string_lossy());
    use hl_gpu::CommandSink as _;
    sink.negotiate(&hl_gpu::FeatureRequest {
        wire_version: hl_gpu::protocol::WIRE_VERSION,
        shader_payloads: 0,
        command_bits: 0,
        texture_formats: 0,
        binding_arrays: 0,
        non_uniform_binding_arrays: 0,
        gpu_features: 0,
    })
    .unwrap();
    sink.submit(&[]).unwrap();

    drop(service);
    assert!(!path.exists());
}

#[test]
fn connection_capacity_is_bounded_and_released() {
    let connections = Connections::new(2);
    let first = connections.acquire().unwrap();
    let second = connections.acquire().unwrap();
    assert!(connections.acquire().is_none());

    drop(first);
    let replacement = connections.acquire().unwrap();
    assert!(connections.acquire().is_none());

    drop(second);
    drop(replacement);
    assert_eq!(connections.active.load(Ordering::Acquire), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn native_present_retains_iosurface_until_consumer_completion() {
    use hl_gpu::protocol::model::descriptor::{SurfaceDesc, TextureDesc};
    use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
    use hl_gpu::{Cmd, GlobalLedger, GpuExecutor, Limits, Session, SystemClock};

    use crate::runtime::gpu::executor::Executors;
    use crate::runtime::presentation::producer::Producer;

    let mut executor = Executors::new(Backend::Wgpu, true).unwrap().executor();
    let limits = Limits::from_capabilities(executor.capabilities());
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(SystemClock::new()),
    );
    let presentations = executor
        .execute(
            &mut session.resources,
            &[
                Cmd::CreateSurface(
                    4,
                    SurfaceDesc {
                        width: 8,
                        height: 6,
                        format: TextureFormat::Bgra8Unorm,
                        token: hl_gpu::SurfaceToken::new(17).unwrap(),
                    },
                ),
                Cmd::CreateTexture(
                    5,
                    TextureDesc {
                        width: 8,
                        height: 6,
                        depth: 1,
                        mip_levels: 1,
                        sample_count: 1,
                        dim: TextureDim::D2,
                        format: TextureFormat::Bgra8Unorm,
                        usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT,
                        label: String::new(),
                    },
                ),
                Cmd::Present {
                    surface: 4,
                    texture: 5,
                    serial: hl_gpu::FrameSerial::new(23).unwrap(),
                },
            ],
        )
        .unwrap();
    let (publisher, frames) = hl_compositor::adapter::smithay::native_frames(1).unwrap();
    let mut producer = Producer::new(publisher);

    producer.publish(&session, &executor, presentations);
    assert!(producer.completion(0).is_none());

    executor
        .execute(&mut session.resources, &[Cmd::DestroyTexture(5)])
        .unwrap();
    assert!(producer.completion(0).is_none());
    drop(frames);
    let completion = producer.completion(0).unwrap();
    assert_eq!(completion.token.get(), 17);
    assert_eq!(completion.serial.get(), 23);
    assert_eq!(
        completion.outcome,
        hl_compositor::adapter::smithay::NativeFrameOutcome::Discarded
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_failure_reservations_deduplicate_exact_present_pairs() {
    use hl_gpu::protocol::model::descriptor::SurfaceDesc;
    use hl_gpu::protocol::model::enums::TextureFormat;
    use hl_gpu::{Cmd, GlobalLedger, GpuExecutor, Limits, Session, SystemClock};

    use crate::runtime::gpu::executor::Executors;
    use crate::runtime::presentation::producer::Producer;

    let executor = Executors::new(Backend::Cpu, false).unwrap().executor();
    let session = Session::new(
        Limits::from_capabilities(executor.capabilities()),
        GlobalLedger::unbounded(),
        Box::new(SystemClock::new()),
    );
    let (publisher, _frames) = hl_compositor::adapter::smithay::native_frames(1).unwrap();
    let producer = Producer::new(publisher);
    let descriptor = SurfaceDesc {
        width: 8,
        height: 6,
        format: TextureFormat::Bgra8Unorm,
        token: hl_gpu::SurfaceToken::new(17).unwrap(),
    };
    let present = Cmd::Present {
        surface: 4,
        texture: 5,
        serial: hl_gpu::FrameSerial::new(23).unwrap(),
    };

    assert_eq!(
        producer.reservations(
            &session,
            &[Cmd::CreateSurface(4, descriptor), present.clone(), present],
        ),
        [(17, 23)]
    );
}
