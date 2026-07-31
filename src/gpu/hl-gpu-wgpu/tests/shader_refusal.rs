//! A shader the wgpu backend cannot accept must be a REFUSED SUBMIT, never a dead service.
//!
//! `wgpu::Device::create_shader_module` reports a validation failure through the device error sink, and
//! with no error scope on the stack that sink's default handler PANICS on the calling thread. The shader
//! payload is guest-controlled — a guest compiles whatever it likes — so an unguarded call turned a shader
//! this backend's own translation emitted in a form wgpu rejects into a panic unwinding out of the GPU
//! connection thread. The Khronos dEQP-GLES2 suite reached it with an ordinary uniform declaration.
//!
//! [`crate::device::Gpu::shader_module`] now wraps every guest-derived module in a validation error scope,
//! so the refusal is the same typed `GpuError` every other backend rejection uses. This battery proves the
//! observable consequences of that over the REAL transport, with a REAL device: the batch is NACKed, the
//! session's resource tables are untouched by the failed batch, the same connection keeps serving, and the
//! listener still accepts a new one.

use std::io;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::CommandSink as _;
use hl_gpu::{
    Cmd, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// A shader whose WGSL translation wgpu REFUSES, reached through the ordinary guest GLSL payload channel.
///
/// The uniform array is padded to a 16-byte std140 stride by `glsl_es::pad_std140_arrays` — except when a
/// local of the same name could shadow the block member, as here, which the pass declines rather than risk
/// reading the uniform where the shader meant the local. naga then emits `array<f32, 4>` (stride 4) in the
/// uniform address space, which wgpu's validator rejects. Any other module wgpu refuses would prove the
/// same property; this one is a shape a guest can actually send.
const REFUSED_FS: &str = "#version 460\n\
layout(std140, binding = 0) uniform HlUniforms { float u[4]; };\n\
layout(location = 0) out vec4 c;\n\
void main() { float u[4]; u[0] = 1.0; c = vec4(u[0]); }\n";

/// A shader the backend accepts, used to prove the connection and the refused id are both still usable.
const ACCEPTED_FS: &str = "#version 460\n\
layout(location = 0) out vec4 c;\n\
void main() { c = vec4(1.0, 0.0, 0.0, 1.0); }\n";

fn shader(id: u32, source: &str) -> Cmd {
    Cmd::CreateShader {
        id,
        kind: ShaderPayloadKind::Glsl,
        spirv: GlslDescriptor {
            stage: glsl_stage::FRAGMENT,
            entry: "main".to_owned(),
            source: source.to_owned(),
        }
        .to_words(),
    }
}

fn buffer(id: u32) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size: 256,
            usage: buffer_usage::STORAGE | buffer_usage::COPY_DST,
            label: String::new(),
        },
    )
}

/// A minimal GPU service over a real socket: one wgpu executor + session per connection, served by the
/// production `hl_gpu` serve loop (whose `HandlerBoundary` is what would otherwise have to contain a panic).
struct Service {
    path: std::path::PathBuf,
    stop: Arc<AtomicBool>,
}

impl Service {
    fn start(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{name}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let accepting = stop.clone();
        thread::spawn(move || {
            while !accepting.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread::spawn(move || {
                            stream.set_nonblocking(false).expect("blocking");
                            let mut executor = WgpuExecutor::new(DeviceConfig::default())
                                .expect("a GPU adapter is required to prove the wgpu executor");
                            let capabilities = executor.capabilities();
                            let mut session = Session::new(
                                Limits::from_capabilities(capabilities.clone()),
                                GlobalLedger::unbounded(),
                                Box::new(FakeClock::new(0)),
                            );
                            let _ = hl_gpu::serve_connection(
                                &stream,
                                &capabilities,
                                |header: &SubmitHeader, batch: &[Cmd]| {
                                    match hl_gpu::runtime::submit(
                                        &mut session,
                                        &mut executor,
                                        header.len as usize,
                                        batch,
                                    ) {
                                        Ok(_) => Verdict::Ack,
                                        Err(_) => Verdict::Nack,
                                    }
                                },
                            );
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { path, stop }
    }

    fn connect(&self) -> hl_gpu::RemoteCommandSink {
        let mut sink = hl_gpu::RemoteCommandSink::new(self.path.to_string_lossy());
        sink.negotiate(&hl_gpu::FeatureRequest {
            wire_version: hl_gpu::protocol::WIRE_VERSION,
            shader_payloads: hl_gpu::protocol::model::capability::shader_payload::GLSL,
            command_bits: 0,
            texture_formats: 0,
            binding_arrays: 0,
            non_uniform_binding_arrays: 0,
            gpu_features: 0,
        })
        .expect("negotiate");
        sink
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn a_shader_the_backend_refuses_fails_only_that_submit() {
    let service = Service::start("hl-wgpu-shader-refusal");
    let mut sink = service.connect();

    sink.submit(&[buffer(1)]).expect("a valid batch is served");
    assert!(
        sink.submit(&[buffer(3), shader(2, REFUSED_FS)]).is_err(),
        "a batch whose shader the backend cannot accept is refused"
    );

    // The refused batch left NO trace: both ids it touched are free again on the SAME connection, which
    // means the service survived the refusal AND the resource tables were rolled back.
    sink.submit(&[buffer(3), shader(2, ACCEPTED_FS)])
        .expect("the connection keeps serving, and the refused batch's ids are free");
    sink.submit(&[Cmd::DestroyBuffer(1), Cmd::DestroyBuffer(3)])
        .expect("the session is still coherent");

    // And the listener still accepts new connections — the refusal did not take the service down.
    let mut second = service.connect();
    second
        .submit(&[buffer(1)])
        .expect("the service still accepts connections after a refused shader");
}

/// The refusal must be a typed error from the executor itself, not a panic contained by the transport's
/// unwind boundary — the two produce the SAME wire byte, so only a direct call can tell them apart.
#[test]
fn the_refusal_is_a_returned_error_not_a_panic() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let capabilities = executor.capabilities();
    let mut session = Session::new(
        Limits::from_capabilities(capabilities),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(&mut session, &mut executor, 0, &[shader(2, REFUSED_FS)])
    }))
    .expect("creating a shader wgpu refuses must NOT panic");
    let error = outcome.expect_err("a shader wgpu refuses must be an error");
    assert!(
        error.to_string().contains("rejected"),
        "the refusal names the rejected module: {error}"
    );

    // The executor is still usable afterwards, on the same device.
    hl_gpu::runtime::submit(&mut session, &mut executor, 0, &[shader(2, ACCEPTED_FS)])
        .expect("the executor keeps working after a refused shader");
}
