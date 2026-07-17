//! CAPSTONE goal #1 — `engine.add(Cuda::new()) / Vulkan::new() / Gl::new()` composes.
//!
//! Construct one `hl_jit::Drivers` registry (the "engine"), attach all THREE real driver plugs via
//! `.add(...)`, then read back each driver's composed `DeviceRequest` and assert the mounts + env it
//! injects into a launch — proving the runtime-neutral driver-plugin seam works with the real CUDA,
//! Vulkan, and GL drivers side by side, ordered, and composing against a shared guest environment.

use std::collections::HashSet;

use hl_jit::{DeviceRequest, Drivers};

use hl_cuda::{Cuda, CudaSpec};
use hl_gl::{Gl, GlSpec};
use hl_vulkan::{Vulkan, VulkanSpec};

/// The host GPU-exec socket every driver binds (host path). The guest sees it at the default
/// `/run/hl-gpu.sock` and `$HL_GPU_EXEC` is set to that guest path.
const HOST_SOCK: &str = "/run/host-gpu.sock";
const GUEST_SOCK: &str = "/run/hl-gpu.sock";
/// The x86-64 guest multiarch libdir all three drivers stage their sonames into.
const LIBDIR: &str = "/usr/lib/x86_64-linux-gnu";

/// True if `req` binds SOMETHING to the guest path `container`.
fn mounts_to(req: &DeviceRequest, container: &str) -> bool {
    req.mounts.iter().any(|m| m.container == container)
}

/// True if `req` binds the exec socket read-WRITE at the guest socket path (the guest connects to it).
fn binds_exec_socket(req: &DeviceRequest) -> bool {
    req.mounts
        .iter()
        .any(|m| m.host == HOST_SOCK && m.container == GUEST_SOCK && !m.read_only)
}

/// True if `req` sets the exact `K=V` env line.
fn has_env(req: &DeviceRequest, kv: &str) -> bool {
    req.env.iter().any(|e| e == kv)
}

#[test]
fn engine_add_composes_all_three_drivers_and_injects_them() {
    // The "engine": an ordered driver registry, empty and inert until drivers are added.
    let mut engine = Drivers::new();
    assert!(engine.is_empty());

    // engine.add(Cuda::new(spec)) / Vulkan::new() / Gl::new() — the goal-line composition. All three
    // point at the same host GPU-exec socket and stage from the same root, for the x86-64 guest arch.
    engine
        .add(Cuda::new(
            CudaSpec::new(hl_cuda::Arch::X86_64, HOST_SOCK)
                .stage_root("/opt/hlroot")
                .advertise("hl Integration GPU", "8.6", 16 << 30),
        ))
        .add(Vulkan::new(
            VulkanSpec::new(hl_vulkan::Arch::X86_64, HOST_SOCK).stage_root("/opt/hlroot"),
        ))
        .add(Gl::new(
            GlSpec::new(hl_gl::Arch::X86_64, HOST_SOCK)
                .stage_root("/opt/hlroot")
                .surface_size(1920, 1080),
        ));

    // Three drivers, in order, with distinct + present names.
    assert_eq!(engine.len(), 3, "all three drivers attached");
    assert_eq!(
        engine.names(),
        vec!["cuda", "vulkan", "gl"],
        "names present and ordered"
    );
    let distinct: HashSet<&str> = engine.names().into_iter().collect();
    assert_eq!(distinct.len(), 3, "all three driver names are distinct");

    // Compose every driver's DeviceRequest against an EXISTING guest env that already has an
    // LD_LIBRARY_PATH, so we prove each driver PREPENDS its libdir rather than clobbering.
    let guest_env = vec![
        "LD_LIBRARY_PATH=/usr/lib:/lib".to_string(),
        "PATH=/usr/bin".to_string(),
    ];
    let reqs = engine.requests(&guest_env);
    assert_eq!(
        reqs.len(),
        3,
        "one request per attached driver, in registry order"
    );
    let (cuda, vk, gl) = (&reqs[0], &reqs[1], &reqs[2]);

    // ---- CUDA (reqs[0]) --------------------------------------------------------------------------
    // device_request injects the THREE guest shim sonames at their guest paths...
    assert!(
        mounts_to(cuda, &format!("{LIBDIR}/libcuda.so.1")),
        "cuda: libcuda.so.1 bound"
    );
    assert!(
        mounts_to(cuda, &format!("{LIBDIR}/libcudart.so.1")),
        "cuda: libcudart.so.1 bound"
    );
    assert!(
        mounts_to(cuda, &format!("{LIBDIR}/libnvidia-ml.so.1")),
        "cuda: libnvidia-ml.so.1 bound"
    );
    // ...binds $HL_GPU_EXEC, prepends the libdir to LD_LIBRARY_PATH, and advertises HL_CUDA_* env.
    assert!(binds_exec_socket(cuda), "cuda: exec socket bound rw");
    assert!(
        has_env(cuda, &format!("HL_GPU_EXEC={GUEST_SOCK}")),
        "cuda: $HL_GPU_EXEC set"
    );
    assert!(
        has_env(cuda, &format!("LD_LIBRARY_PATH={LIBDIR}:/usr/lib:/lib")),
        "cuda: guest libdir prepended to the existing LD_LIBRARY_PATH",
    );
    assert!(
        has_env(cuda, "HL_CUDA_NAME=hl Integration GPU"),
        "cuda: HL_CUDA_NAME advertised"
    );
    assert!(
        has_env(cuda, "HL_CUDA_CC=8.6"),
        "cuda: HL_CUDA_CC advertised"
    );
    assert!(
        has_env(cuda, "HL_CUDA_VRAM_BYTES=17179869184"),
        "cuda: HL_CUDA_VRAM_BYTES advertised"
    );
    assert!(
        !cuda.render_node,
        "cuda: injects libs, not a synthetic render node"
    );

    // ---- VULKAN (reqs[1]) ------------------------------------------------------------------------
    // The ICD is bound at both its versioned + unversioned soname, plus its icd.json manifest...
    assert!(
        mounts_to(vk, &format!("{LIBDIR}/libvk_hl.so.1")),
        "vulkan: libvk_hl.so.1 bound"
    );
    assert!(
        mounts_to(vk, &format!("{LIBDIR}/libvk_hl.so")),
        "vulkan: unversioned libvk_hl.so bound"
    );
    assert!(
        mounts_to(vk, &format!("{LIBDIR}/hl_vulkan_icd.json")),
        "vulkan: icd.json dropped"
    );
    // ...and the loader is pointed at that icd.json via VK_ICD_FILENAMES (+ the socket + LD_LIBRARY_PATH).
    assert!(binds_exec_socket(vk), "vulkan: exec socket bound rw");
    assert!(
        has_env(vk, &format!("VK_ICD_FILENAMES={LIBDIR}/hl_vulkan_icd.json")),
        "vulkan: VK_ICD_FILENAMES points the loader at the dropped icd.json",
    );
    assert!(
        has_env(vk, &format!("HL_GPU_EXEC={GUEST_SOCK}")),
        "vulkan: $HL_GPU_EXEC set"
    );
    assert!(
        has_env(vk, &format!("LD_LIBRARY_PATH={LIBDIR}:/usr/lib:/lib")),
        "vulkan: guest libdir prepended to the existing LD_LIBRARY_PATH",
    );
    assert!(
        !vk.render_node,
        "vulkan: injects an ICD, not a synthetic render node"
    );

    // ---- GL (reqs[2]) ----------------------------------------------------------------------------
    // Both GLES/EGL sonames are bound (GLESv2 is the DT_NEEDED->libEGL forwarding stub)...
    assert!(
        mounts_to(gl, &format!("{LIBDIR}/libEGL.so.1")),
        "gl: libEGL.so.1 bound"
    );
    assert!(
        mounts_to(gl, &format!("{LIBDIR}/libGLESv2.so.2")),
        "gl: libGLESv2.so.2 bound"
    );
    // ...plus the socket, the prepended LD_LIBRARY_PATH, and the advertised surface size.
    assert!(binds_exec_socket(gl), "gl: exec socket bound rw");
    assert!(
        has_env(gl, &format!("HL_GPU_EXEC={GUEST_SOCK}")),
        "gl: $HL_GPU_EXEC set"
    );
    assert!(
        has_env(gl, &format!("LD_LIBRARY_PATH={LIBDIR}:/usr/lib:/lib")),
        "gl: guest libdir prepended to the existing LD_LIBRARY_PATH",
    );
    assert!(
        has_env(gl, "HL_GL_SURFACE_W=1920"),
        "gl: HL_GL_SURFACE_W advertised"
    );
    assert!(
        has_env(gl, "HL_GL_SURFACE_H=1080"),
        "gl: HL_GL_SURFACE_H advertised"
    );
    assert!(
        !gl.render_node,
        "gl: injects shim libs, not a synthetic render node"
    );

    // All three converge on the SAME guest exec socket path — the one host GPU-exec seam they all speak.
    for req in [cuda, vk, gl] {
        assert!(
            binds_exec_socket(req),
            "every driver binds the shared $HL_GPU_EXEC socket"
        );
        assert!(
            has_env(req, &format!("HL_GPU_EXEC={GUEST_SOCK}")),
            "every driver names the same socket"
        );
    }
}
