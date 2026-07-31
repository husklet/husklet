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

/// A batch the executor cannot lower must fail THAT batch only. The guest program that sent it is told
/// (`submit` returns an error, which the driver surfaces on its own error path); the session, the
/// connection and the listener all keep serving. This is the isolation property: one guest's bad shader
/// cannot end the workspace session everything else in it is mapped into.
#[test]
fn a_rejected_submit_fails_only_that_submit() {
    use hl_gpu::protocol::model::descriptor::BufferDesc;
    use hl_gpu::protocol::model::enums::buffer_usage;
    use hl_gpu::protocol::model::kernel::KernelDescriptor;
    use hl_gpu::CommandSink as _;
    use hl_gpu::{Cmd, ShaderPayloadKind};

    let path = std::env::temp_dir().join(format!("hl-gpu-nack-{}.sock", std::process::id()));
    let service = Service::start(&path, Configuration::new(Backend::Cpu, false)).unwrap();

    let connect = || {
        let mut sink = hl_gpu::RemoteCommandSink::new(path.to_string_lossy());
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
        sink
    };
    let buffer = |id| {
        Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: 256,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_DST,
                label: String::new(),
            },
        )
    };
    // Source the host's translator must reject: the same `GpuError::Kernel` a GLSL shader that naga cannot
    // lower produces, reached through the production kernel compiler.
    let unlowerable = Cmd::CreateShader {
        id: 2,
        kind: ShaderPayloadKind::PtxKernel,
        spirv: KernelDescriptor {
            ptx: "this is not PTX and never will be".to_owned(),
            entry: "main".to_owned(),
            block: [1, 1, 1],
        }
        .to_words(),
    };

    let mut sink = connect();
    sink.submit(&[buffer(1)]).unwrap();
    assert!(
        sink.submit(&[buffer(3), unlowerable]).is_err(),
        "a batch the host cannot lower is rejected"
    );

    // Still serving on the SAME connection, and the rejected batch left no trace: id 3 is free again.
    sink.submit(&[buffer(3)])
        .expect("the connection keeps serving after a rejected submit");
    sink.submit(&[Cmd::DestroyBuffer(1), Cmd::DestroyBuffer(3)])
        .unwrap();

    // And the listener still accepts new connections — the rejection did not take the service down.
    let mut second = connect();
    second
        .submit(&[buffer(1)])
        .expect("the service still accepts connections after a rejected submit");

    drop(service);
    assert!(!path.exists());
}

/// The real-world shape of the same defect: a guest GLSL shader naga cannot lower (here, a constant index
/// into a scalar — the `InvalidAccess { indexed: true }` class GTK4's GSK renderer produced). The
/// translation failure must NACK the submit and leave the service serving.
#[cfg(target_os = "macos")]
#[test]
fn an_unlowerable_guest_shader_does_not_end_the_session() {
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    use hl_gpu::CommandSink as _;
    use hl_gpu::{Cmd, ShaderPayloadKind};

    let path = std::env::temp_dir().join(format!("hl-gpu-glsl-{}.sock", std::process::id()));
    let Ok(service) = Service::start(&path, Configuration::new(Backend::Wgpu, false)) else {
        return; // no host device on this machine; the CPU-backed test covers the property
    };

    let mut sink = hl_gpu::RemoteCommandSink::new(path.to_string_lossy());
    sink.negotiate(&hl_gpu::FeatureRequest {
        wire_version: hl_gpu::protocol::WIRE_VERSION,
        shader_payloads: hl_gpu::protocol::model::capability::shader_payload::GLSL,
        command_bits: 0,
        texture_formats: 0,
        binding_arrays: 0,
        non_uniform_binding_arrays: 0,
        gpu_features: 0,
    })
    .unwrap();

    let shader = |id, source: &str| Cmd::CreateShader {
        id,
        kind: ShaderPayloadKind::Glsl,
        spirv: GlslDescriptor {
            stage: glsl_stage::FRAGMENT,
            entry: "main".to_owned(),
            source: source.to_owned(),
        }
        .to_words(),
    };
    let unlowerable = r#"#version 460
layout(location = 0) out vec4 color;
void main() {
    float s = 1.0;
    color = vec4(s[2], 0.0, 0.0, 1.0);
}
"#;
    let sound = r#"#version 460
layout(location = 0) out vec4 color;
void main() { color = vec4(1.0, 0.0, 0.0, 1.0); }
"#;

    assert!(
        sink.submit(&[shader(1, unlowerable)]).is_err(),
        "a shader the host translator rejects NACKs its submit"
    );
    sink.submit(&[shader(1, sound)])
        .expect("the session keeps serving, and the rejected shader's id is free again");

    drop(service);
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
