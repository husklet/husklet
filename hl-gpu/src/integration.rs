//! The dd-gpu-side implementor of dd-jit's runtime-neutral device seam ([`hl_jit::DeviceProvider`]).
//!
//! ALL the GPU/CUDA/display specifics that used to be threaded through the container launcher live here
//! and here only: which host libraries/binaries/sockets to inject, at which guest paths, behind which env
//! vars, and whether an accelerated render node is needed. dd-jit / dd-jit-darwin stay device-agnostic —
//! they receive a plain [`hl_jit::DeviceRequest`] (mounts + env + a render-node bool) and apply it
//! generically, never referencing CUDA, IOSurface, or Wayland.
//!
//! The caller (dd-cli) owns *where host files live* (it resolves the `~/.dd/...` drop-in paths and the
//! guest ISA); this module owns *how a GPU maps into a guest* (target lib dir, guest socket paths, env
//! contract, LD_LIBRARY_PATH composition). Construct a [`GpuIntegration`] from a workspace's gui/cuda
//! config + resolved host paths, then hand it to the dd-jit builder via
//! `builder.apply_device(&provider.device_request(&env))`.
//!
//! Gated behind the `runtime` cargo feature (which pulls in `dd-jit`); the crate's pure-`std`,
//! headless-testable IR/wire core builds and tests without it.

use hl_jit::{DeviceMount, DeviceProvider, DeviceRequest};

/// The guest ISA the injected shims/libraries must match (selects the multiarch lib dir the runtime
/// mounts into).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GuestArch {
    /// `x86_64-linux-gnu`.
    X86_64,
    /// `aarch64-linux-gnu`.
    Aarch64,
}

impl GuestArch {
    /// The guest multiarch library directory injected shared objects are bound into.
    fn libdir(self) -> &'static str {
        match self {
            GuestArch::X86_64 => "/usr/lib/x86_64-linux-gnu",
            GuestArch::Aarch64 => "/usr/lib/aarch64-linux-gnu",
        }
    }
}

/// The accelerated-display (`--gui`) integration: bind the host compositor + GPU-command sockets and the
/// client-lib / demo-binary drop-ins into the guest, advertise them via env, and request the render node.
/// Empty `lib_dir`/`bin_dir` = no drop-ins for that slot (nothing bound).
#[derive(Clone, Debug, Default)]
pub struct DisplayIntegration {
    /// Host path of the compositor (Wayland/DDP) socket; bound rw at `/run/user/0/wayland-0`.
    pub wayland_sock: String,
    /// Host path of the dd-gpu IR executor socket; bound rw at `/run/user/0/dd-gpu-0`.
    pub gpu_exec_sock: String,
    /// Host dir of client `*.so*` drop-ins (each bound into the guest multiarch lib dir). Empty = none.
    pub lib_dir: String,
    /// Host dir of demo/test binary drop-ins (each bound into `/usr/local/bin/`). Empty = none.
    pub bin_dir: String,
    /// Host path of the workspace overlay UPPER's copy of the guest multiarch lib dir (e.g.
    /// `<upper>/usr/lib/aarch64-linux-gnu`). When set, [`device_request`](GpuIntegration::device_request)
    /// self-heals the overlay by pruning stale 0-byte injection stubs left there (see
    /// [`prune_shadowing_stubs`]). Empty = skip pruning (headless/tests, or a non-overlay launch).
    pub overlay_lib_dir: String,
}

/// The simulated-CUDA-device integration: inject dd's NVML shim (+ the real `nvidia-smi`) so unmodified
/// probes see an NVIDIA-looking device. `nvml_so` / `nvidia_smi` empty = that artifact is absent and
/// simply not injected (the caller is responsible for any user-facing warning).
#[derive(Clone, Debug, Default)]
pub struct CudaIntegration {
    /// Reported device name (→ `HL_CUDA_NAME`).
    pub name: String,
    /// Reported compute capability `"major.minor"` (→ `HL_CUDA_CC`).
    pub compute_capability: String,
    /// Reported VRAM in MB (→ `HL_CUDA_VRAM`).
    pub vram_mb: u32,
    /// Host path of `libnvidia-ml.so.1` (bound at the guest lib dir under both the versioned and
    /// unversioned names). Empty = not injected.
    pub nvml_so: String,
    /// Host path of the real `nvidia-smi` binary (bound at `/usr/local/bin/nvidia-smi`). Empty = not injected.
    pub nvidia_smi: String,
    /// The guest's existing `LD_LIBRARY_PATH` (from the workspace's own env) to prepend the shim lib dir
    /// to, if any. `None`/empty = the lib dir alone.
    pub prior_ld_library_path: Option<String>,
}

/// The full GPU integration for one container launch — display and/or CUDA. Construct it from the
/// workspace's config plus the host paths the caller resolved, then use it as a [`DeviceProvider`].
#[derive(Clone, Debug)]
pub struct GpuIntegration {
    /// The guest ISA (selects the target multiarch lib dir).
    pub arch: GuestArch,
    /// Accelerated-display integration, if the workspace is `--gui`.
    pub display: Option<DisplayIntegration>,
    /// Simulated-CUDA-device integration, if the workspace configures a `cuda` device.
    pub cuda: Option<CudaIntegration>,
}

impl GpuIntegration {
    /// A GPU integration for `arch` with neither display nor CUDA armed (inert — produces an empty
    /// [`DeviceRequest`]). Set [`display`](Self::display) / [`cuda`](Self::cuda) as needed.
    pub fn new(arch: GuestArch) -> Self {
        GpuIntegration { arch, display: None, cuda: None }
    }
    /// Arm the accelerated-display integration.
    pub fn with_display(mut self, d: DisplayIntegration) -> Self {
        self.display = Some(d);
        self
    }
    /// Arm the simulated-CUDA-device integration.
    pub fn with_cuda(mut self, c: CudaIntegration) -> Self {
        self.cuda = Some(c);
        self
    }
    /// `true` when neither display nor CUDA is armed — the caller can skip applying it entirely.
    pub fn is_inert(&self) -> bool {
        self.display.is_none() && self.cuda.is_none()
    }
}

/// Whether the accelerated-display shim is *authoritative* for a drop-in shared object and may bind it
/// over the guest's multiarch lib dir. The shim owns the GPU/GL render stack (EGL/GLES/GL/GBM/Vulkan/CUDA,
/// including the GLVND dispatchers and ANGLE's `libEGL`/`libGLESv2` sonames), the Wayland client transport
/// it speaks to dd-display with (`libwayland-*`), and dd's own shim cores. Every other `*.so` in the
/// drop-in dir belongs to the guest distro and MUST NOT be shadowed — above all `libX11`/`libxcb` (GTK/Qt
/// X11 backends) and the base C/C++ runtime (`libstdc++`, `libz`, `libc`, …). Binding a stub over the
/// guest's real copy is the "shim library shadowing" bug (docs/goal.md): the app then loads a crippled
/// library and used to need an `LD_PRELOAD` workaround. Restricting the bind set to what the shim actually
/// provides makes the guest resolve its own libraries with no hack — the shim libs the guest lacks
/// (EGL/GLES/GBM/Vulkan/CUDA/dd-shim) are exactly the ones matched here, while shared base deps
/// (libX11/libffi/libstdc++/…) that any GUI image already ships are left to the distro.
fn shim_owns_lib(file_name: &str) -> bool {
    // The soname "stem": everything before the first ".so" (`libEGL.so.1` -> `libegl`).
    let stem = file_name.split(".so").next().unwrap_or(file_name).to_ascii_lowercase();
    // Families with a distinctive prefix that never collides with an unrelated distro soname.
    const OWNED_PREFIXES: &[&str] = &[
        "libegl",      // libEGL, libEGL_mesa, ANGLE's libEGL
        "libgles",     // libGLESv1_CM, libGLESv2
        "libvulkan",   // Vulkan loader + dd/lavapipe ICD
        "libvklayer",  // Vulkan layers
        "libcuda",     // CUDA driver shim (libcuda, libcudart)
        "libwayland-", // wayland-egl platform + client/cursor transport to dd-display
        "libdd",       // dd's own shim cores (libdd*, libddshim)
    ];
    if OWNED_PREFIXES.iter().any(|p| stem.starts_with(p)) {
        return true;
    }
    // GL-family sonames matched EXACTLY — a bare `libgl` prefix would wrongly capture distro libs such as
    // `libglib-2.0`, so enumerate the real GL/GLVND/GBM stems instead.
    const OWNED_EXACT: &[&str] = &["libgl", "libglu", "libglx", "libgldispatch", "libopengl", "libglapi", "libgbm"];
    if OWNED_EXACT.contains(&stem.as_str()) {
        return true;
    }
    // dd shim cores whose name doesn't start with `libdd` (e.g. `gl_shim` / `*_shim` builds).
    stem.contains("shim")
}

/// Self-heal a workspace overlay that still carries stale **injection stubs**, so the "shim library
/// shadowing" failure class (docs/goal.md) can't recur from legacy overlay state.
///
/// dd injects the render stack by bind-mounting shim libs into the guest multiarch dir and PREPENDING
/// that dir to `LD_LIBRARY_PATH`. A pre-`5e8c10ee` "inject every `*.so`" build could leave, in the
/// workspace overlay UPPER, a full set of ZERO-BYTE bind-mount-target stubs (`libz.so.1`, `libffi.so.8`,
/// `libstdc++.so.6`, …). Once the inject set shrank to only what [`shim_owns_lib`] covers, those stubs
/// stopped being covered by a real mount — yet, being first on `LD_LIBRARY_PATH`, an empty file there
/// SHADOWS the guest's real lib, so the loader hits a 0-byte file and returns `ENOEXEC`
/// ("Exec format error", cascading to EXIT=127). This prunes such orphans from the overlay upper so the
/// guest resolves its OWN base libs again, with no re-injection and no `LD_PRELOAD` hack.
///
/// It also heals a second, ABI-level variant of the same class: a stale render-stack shim left in the
/// overlay upper by a PRIOR launch of a DIFFERENT-libc guest (a musl `libwayland-egl.so.1` from a Chrome
/// session shadowing a later GTK/glibc guest, → `libc.musl-<arch>.so.1: cannot open shared object file`).
/// Such a shim is a dd inject artifact (the guest's real base libs live in the image rootfs, never the
/// writable upper), so if we are NOT binding it this run it can only shadow the guest's real lib and is
/// pruned regardless of size. The launcher now injects the ABI-matching variant, so this is defense in
/// depth against overlays dirtied before that fix.
///
/// Safe by construction — a candidate is pruned ONLY when it is:
///   * inside `overlay_lib_dir` (the workspace overlay upper's multiarch dir) — never the shared image
///     rootfs, which this function is never handed;
///   * a REGULAR file (never a dir or a symlink — `symlink_metadata` doesn't follow links out of the
///     upper);
///   * a shared-object name (`*.so*`);
///   * NOT a soname we bind a real lib over this run (`bound_this_run`) — that one is already covered; and
///   * EITHER exactly 0 bytes (a leftover bind-mount-target stub of any soname) OR a render-stack shim we
///     own ([`shim_owns_lib`]) that we are not binding this run (a stale mismatched-ABI inject).
///
/// Returns the pruned sonames (sorted), for logging and tests.
fn prune_shadowing_stubs(
    overlay_lib_dir: &str,
    bound_this_run: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut pruned = Vec::new();
    let rd = match std::fs::read_dir(overlay_lib_dir) {
        Ok(rd) => rd,
        // No overlay multiarch dir yet (fresh workspace) or unreadable => nothing to heal.
        Err(_) => return pruned,
    };
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if !name.contains(".so") {
            continue;
        }
        // A real lib we're binding over this run is already covered — leave the stub alone.
        if bound_this_run.contains(name.as_ref()) {
            continue;
        }
        let path = ent.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Only a REGULAR file in the upper (never a dir or a symlink out of the upper).
        if !meta.file_type().is_file() {
            continue;
        }
        // Two orphan shapes shadow the guest's real lib (first on LD_LIBRARY_PATH) and must be pruned:
        //   * a ZERO-BYTE stub of ANY soname — a leftover bind-mount target from the pre-`5e8c10ee`
        //     "inject every .so" era; the loader hits an empty file -> ENOEXEC (EXIT=127).
        //   * a render-stack shim we OWN ([`shim_owns_lib`]) that we are NOT binding this run — a stale
        //     inject from a PRIOR launch of a DIFFERENT-libc guest, e.g. a musl `libwayland-egl.so.1`
        //     left by a Chrome (musl) session now shadowing a GTK (glibc) guest, which fails the loader
        //     with `libc.musl-<arch>.so.1: cannot open shared object file`. A shim-owned lib in the
        //     overlay UPPER is always a dd inject artifact — the guest's real base libs live in the image
        //     rootfs, never the writable upper — so removing an unbound one only lets the guest resolve
        //     its own lib. (Any shim we DO inject this run is in `bound_this_run` and skipped above; its
        //     RO bind-mount covers the path regardless.)
        let is_zero_stub = meta.len() == 0;
        let is_stale_shim = shim_owns_lib(&name);
        if !is_zero_stub && !is_stale_shim {
            continue;
        }
        let reason = if is_zero_stub {
            "an empty file first on LD_LIBRARY_PATH shadowed the guest's real lib -> ENOEXEC"
        } else {
            "a stale mismatched-ABI render shim first on LD_LIBRARY_PATH shadowed the guest's real lib -> wrong-libc load failure"
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {
                eprintln!(
                    "[dd-gpu] pruned stale inject stub {} from the workspace overlay upper ({reason})",
                    path.display()
                );
                pruned.push(name.into_owned());
            }
            Err(e) => {
                eprintln!("[dd-gpu] could not prune stale inject stub {}: {e}", path.display());
            }
        }
    }
    pruned.sort();
    pruned
}

impl DeviceProvider for GpuIntegration {
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest {
        let mut req = DeviceRequest::default();
        let libdir = self.arch.libdir();

        // ---- Accelerated display (--gui): sockets + client drop-ins + the render node. ----
        if let Some(d) = &self.display {
            // The engine's host-IOSurface GPU path (render-node synth + host-backed alloc ioctl).
            req.render_node = true;
            // The compositor socket + its env contract.
            req.mounts.push(DeviceMount::rw(d.wayland_sock.clone(), "/run/user/0/wayland-0"));
            req.env.push("WAYLAND_DISPLAY=wayland-0".to_string());
            req.env.push("XDG_RUNTIME_DIR=/run/user/0".to_string());
            // The dd-gpu IR executor socket the guest streams GPU commands to.
            req.mounts.push(DeviceMount::rw(d.gpu_exec_sock.clone(), "/run/user/0/dd-gpu-0"));
            req.env.push("HL_GPU_EXEC=/run/user/0/dd-gpu-0".to_string());
            // Mount-not-bake the shim's OWN runtime libs (the GPU/GL render stack + the Wayland client
            // transport it speaks to dd-display with): each such *.so* in the drop-in dir is bound over the
            // guest multiarch lib dir, and that dir is prepended to LD_LIBRARY_PATH so a bare image works.
            // We bind ONLY the libraries the shim is authoritative for (`shim_owns_lib`) — never unrelated
            // distro libraries that happen to share the drop-in dir. Binding a stub `libX11.so.6` over the
            // guest's real one is the "shim library shadowing" bug: GTK/Qt X11 apps then load a crippled
            // libX11 (previously papered over with an `LD_PRELOAD`). Filtering here lets the guest resolve
            // its own libX11/libstdc++/libz with no workaround. When the dir is present we always advertise
            // the lib dir on the path (even if nothing matched), matching the launcher's original behavior.
            // Track the sonames we bind a real render lib over this run, so the overlay stub-prune below
            // never disturbs one we're already covering.
            let mut bound: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            if !d.lib_dir.is_empty() {
                if let Ok(rd) = std::fs::read_dir(&d.lib_dir) {
                    for ent in rd.flatten() {
                        let name = ent.file_name();
                        let name = name.to_string_lossy();
                        if name.contains(".so") && shim_owns_lib(&name) {
                            bound.insert(name.clone().into_owned());
                            req.mounts.push(DeviceMount::ro(
                                ent.path().to_string_lossy().into_owned(),
                                format!("{libdir}/{name}"),
                            ));
                        }
                    }
                }
                let prior = guest_env
                    .iter()
                    .rev()
                    .find_map(|e| e.strip_prefix("LD_LIBRARY_PATH=").map(str::to_string));
                let ldp = match prior {
                    Some(v) if !v.is_empty() => format!("{libdir}:{v}"),
                    _ => libdir.to_string(),
                };
                req.env.push(format!("LD_LIBRARY_PATH={ldp}"));
            }
            // Durable self-heal: prune stale 0-byte injection stubs a legacy overlay may still carry in
            // the guest multiarch dir (empty files there shadow the guest's real libs on LD_LIBRARY_PATH
            // -> ENOEXEC). Runs every launch so no manual overlay repair is ever needed; a no-op once the
            // overlay is clean. Empty `overlay_lib_dir` (headless/tests) => skipped.
            if !d.overlay_lib_dir.is_empty() {
                prune_shadowing_stubs(&d.overlay_lib_dir, &bound);
            }
            // Mount-not-bake any GUI demo/test binaries into /usr/local/bin so a bare image can run a real
            // Wayland client.
            if !d.bin_dir.is_empty() {
                if let Ok(rd) = std::fs::read_dir(&d.bin_dir) {
                    for ent in rd.flatten() {
                        let name = ent.file_name();
                        let name = name.to_string_lossy();
                        req.mounts.push(DeviceMount::ro(
                            ent.path().to_string_lossy().into_owned(),
                            format!("/usr/local/bin/{name}"),
                        ));
                    }
                }
            }
        }

        // ---- Simulated CUDA device: NVML shim + real nvidia-smi + the reported-device env. ----
        if let Some(c) = &self.cuda {
            // The shim seeds its reported device from these (always advertised, even if the shim itself is
            // missing — matching the launcher's original ordering).
            req.env.push(format!("HL_CUDA_NAME={}", c.name));
            req.env.push(format!("HL_CUDA_CC={}", c.compute_capability));
            req.env.push(format!("HL_CUDA_VRAM={}", c.vram_mb));
            if !c.nvml_so.is_empty() {
                // Inject the NVML shim under both the versioned and unversioned names (some callers dlopen
                // the bare name), and point the loader at OUR lib dir first.
                req.mounts.push(DeviceMount::ro(c.nvml_so.clone(), format!("{libdir}/libnvidia-ml.so.1")));
                req.mounts.push(DeviceMount::ro(c.nvml_so.clone(), format!("{libdir}/libnvidia-ml.so")));
                let ldp = match c.prior_ld_library_path.as_deref() {
                    Some(v) if !v.is_empty() => format!("{libdir}:{v}"),
                    _ => libdir.to_string(),
                };
                req.env.push(format!("LD_LIBRARY_PATH={ldp}"));
            }
            if !c.nvidia_smi.is_empty() {
                req.mounts.push(DeviceMount::ro(c.nvidia_smi.clone(), "/usr/local/bin/nvidia-smi"));
            }
        }

        req
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_integration_produces_empty_request() {
        let req = GpuIntegration::new(GuestArch::Aarch64).device_request(&[]);
        assert_eq!(req, DeviceRequest::default());
        assert!(GpuIntegration::new(GuestArch::Aarch64).is_inert());
    }

    #[test]
    fn display_only_sockets_env_and_render_node() {
        // No lib/bin drop-in dirs → no fs access, no LD_LIBRARY_PATH; just the two sockets + their env.
        let g = GpuIntegration::new(GuestArch::X86_64).with_display(DisplayIntegration {
            wayland_sock: "/host/run/wayland-0".into(),
            gpu_exec_sock: "/host/run/dd-gpu.sock".into(),
            lib_dir: String::new(),
            bin_dir: String::new(),
            overlay_lib_dir: String::new(),
        });
        let req = g.device_request(&[]);
        assert!(req.render_node);
        assert_eq!(
            req.mounts,
            vec![
                DeviceMount::rw("/host/run/wayland-0", "/run/user/0/wayland-0"),
                DeviceMount::rw("/host/run/dd-gpu.sock", "/run/user/0/dd-gpu-0"),
            ]
        );
        assert_eq!(
            req.env,
            vec![
                "WAYLAND_DISPLAY=wayland-0".to_string(),
                "XDG_RUNTIME_DIR=/run/user/0".to_string(),
                "HL_GPU_EXEC=/run/user/0/dd-gpu-0".to_string(),
            ]
        );
    }

    #[test]
    fn cuda_only_injects_shim_and_reports_device() {
        let g = GpuIntegration::new(GuestArch::X86_64).with_cuda(CudaIntegration {
            name: "dd Metal (CUDA-sim) Device".into(),
            compute_capability: "8.6".into(),
            vram_mb: 4096,
            nvml_so: "/host/nvml/libnvidia-ml.so.1".into(),
            nvidia_smi: "/host/bin/nvidia-smi".into(),
            prior_ld_library_path: None,
        });
        let req = g.device_request(&[]);
        assert!(!req.render_node); // CUDA presence does NOT arm the render node
        assert_eq!(
            req.mounts,
            vec![
                DeviceMount::ro("/host/nvml/libnvidia-ml.so.1", "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1"),
                DeviceMount::ro("/host/nvml/libnvidia-ml.so.1", "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so"),
                DeviceMount::ro("/host/bin/nvidia-smi", "/usr/local/bin/nvidia-smi"),
            ]
        );
        assert_eq!(
            req.env,
            vec![
                "HL_CUDA_NAME=dd Metal (CUDA-sim) Device".to_string(),
                "HL_CUDA_CC=8.6".to_string(),
                "HL_CUDA_VRAM=4096".to_string(),
                "LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu".to_string(),
            ]
        );
    }

    #[test]
    fn cuda_missing_shim_still_reports_device_but_no_ld_or_bind() {
        // Empty nvml_so/nvidia_smi → nothing bound, no LD_LIBRARY_PATH, but DD_CUDA_* still advertised.
        let g = GpuIntegration::new(GuestArch::Aarch64).with_cuda(CudaIntegration {
            name: "X".into(),
            compute_capability: "7.5".into(),
            vram_mb: 2048,
            nvml_so: String::new(),
            nvidia_smi: String::new(),
            prior_ld_library_path: None,
        });
        let req = g.device_request(&[]);
        assert!(req.mounts.is_empty());
        assert_eq!(
            req.env,
            vec!["HL_CUDA_NAME=X".to_string(), "HL_CUDA_CC=7.5".to_string(), "HL_CUDA_VRAM=2048".to_string()]
        );
    }

    #[test]
    fn cuda_ld_prepends_existing_value() {
        let g = GpuIntegration::new(GuestArch::Aarch64).with_cuda(CudaIntegration {
            name: "X".into(),
            compute_capability: "8.0".into(),
            vram_mb: 1,
            nvml_so: "/n/libnvidia-ml.so.1".into(),
            nvidia_smi: String::new(),
            prior_ld_library_path: Some("/opt/lib".into()),
        });
        let req = g.device_request(&[]);
        assert!(req.env.contains(&"LD_LIBRARY_PATH=/usr/lib/aarch64-linux-gnu:/opt/lib".to_string()));
    }

    #[test]
    fn display_ld_prepends_last_existing_env_value() {
        // With a lib_dir present, the display path composes against the LAST LD_LIBRARY_PATH in guest_env.
        // Use a temp dir so read_dir succeeds; leave it empty so only the LD env line is asserted.
        let dir = std::env::temp_dir().join(format!("ddgpu-int-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let g = GpuIntegration::new(GuestArch::X86_64).with_display(DisplayIntegration {
            wayland_sock: "/w".into(),
            gpu_exec_sock: "/e".into(),
            lib_dir: dir.to_string_lossy().into_owned(),
            bin_dir: String::new(),
            overlay_lib_dir: String::new(),
        });
        let env = vec!["LD_LIBRARY_PATH=/a".to_string(), "LD_LIBRARY_PATH=/b".to_string()];
        let req = g.device_request(&env);
        assert!(req.env.contains(&"LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu:/b".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shim_owns_only_render_stack_never_distro_libs() {
        // The shim is authoritative for the GPU/GL/Vulkan/Wayland render stack + dd's own cores…
        for owned in [
            "libEGL.so.1",
            "libEGL_mesa.so.0",
            "libGLESv2.so.2",
            "libGLESv1_CM.so.1",
            "libGL.so.1",
            "libGLdispatch.so.0",
            "libOpenGL.so.0",
            "libglapi.so.0",
            "libgbm.so.1",
            "libvulkan.so.1",
            "libvulkan_dd.so",
            "libcuda.so.1",
            "libwayland-egl.so.1",
            "libwayland-client.so.0",
            "libwayland-cursor.so.0",
            "libddshim.so",
            "libgl_shim.so",
        ] {
            assert!(shim_owns_lib(owned), "{owned} should be shim-owned");
        }
        // …and MUST NOT claim ownership of any unrelated distro library sharing the drop-in dir. libX11 is
        // the headline case (the shadowing that broke GTK/Qt X11 backends); the rest guard the general trap.
        for distro in [
            "libX11.so.6",
            "libX11-xcb.so.1",
            "libxcb.so.1",
            "libXext.so.6",
            "libstdc++.so.6",
            "libgcc_s.so.1",
            "libz.so.1",
            "libc.so.6",
            "libm.so.6",
            "libffi.so.8",
            "libglib-2.0.so.0", // the greedy-`libgl`-prefix trap
            "libgio-2.0.so.0",
        ] {
            assert!(!shim_owns_lib(distro), "{distro} must be left to the distro, not shadowed");
        }
    }

    #[test]
    fn display_binds_render_libs_but_not_shadowing_distro_libs() {
        // End-to-end through device_request: a drop-in dir holding BOTH shim render libs and distro libs
        // (as a mis-assembled `~/.dd/gui/<arch>/lib` would) binds only the render libs; the distro libX11 &
        // friends are never mounted, so the guest resolves its own real ones with no LD_PRELOAD.
        let dir = std::env::temp_dir().join(format!("ddgpu-shadow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "libEGL.so.1",
            "libGLESv2.so.2",
            "libwayland-egl.so.1",
            "libX11.so.6",    // the stub that must be skipped
            "libstdc++.so.6", // distro runtime, must be skipped
            "libz.so.1",      // distro base lib, must be skipped
            "notalib.txt",    // non-.so, ignored
        ] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let g = GpuIntegration::new(GuestArch::Aarch64).with_display(DisplayIntegration {
            wayland_sock: "/w".into(),
            gpu_exec_sock: "/e".into(),
            lib_dir: dir.to_string_lossy().into_owned(),
            bin_dir: String::new(),
            overlay_lib_dir: String::new(),
        });
        let req = g.device_request(&[]);
        let bound: Vec<&str> = req.mounts.iter().map(|m| m.container.as_str()).collect();
        // The three render libs are bound over the guest multiarch dir…
        assert!(bound.contains(&"/usr/lib/aarch64-linux-gnu/libEGL.so.1"));
        assert!(bound.contains(&"/usr/lib/aarch64-linux-gnu/libGLESv2.so.2"));
        assert!(bound.contains(&"/usr/lib/aarch64-linux-gnu/libwayland-egl.so.1"));
        // …and NOTHING shadows the guest's real libX11 / libstdc++ / libz.
        assert!(
            !bound.iter().any(|p| p.contains("libX11") || p.contains("libstdc++") || p.contains("libz.so")),
            "distro libs must not be shadowed, got {bound:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_request_prunes_stale_zero_byte_overlay_stubs_only() {
        // End-to-end through device_request, reproducing the cold-Chrome ENOEXEC failure class: a legacy
        // workspace overlay UPPER whose multiarch dir still holds pre-5e8c10ee 0-byte inject stubs
        // (libz/libffi/libstdc++, and even a 0-byte libEGL) alongside the guest's OWN real base libs.
        // dd binds the render stack from a separate drop-in dir and, this run, must prune the orphaned
        // 0-byte stubs from the overlay so the guest resolves its real base libs — while leaving the real
        // (non-empty) distro libs, non-.so files, and any stub it actually covers with a bind untouched.
        let base = std::env::temp_dir().join(format!("ddgpu-prune-{}", std::process::id()));
        let dropin = base.join("dropin"); // host ~/.dd/gui/<arch>/lib
        let overlay = base.join("overlay"); // workspace overlay upper's /usr/lib/<arch>
        std::fs::create_dir_all(&dropin).unwrap();
        std::fs::create_dir_all(&overlay).unwrap();

        // Real render shim to inject (non-empty content).
        std::fs::write(dropin.join("libEGL.so.1"), b"\x7fELF-real-egl").unwrap();
        std::fs::write(dropin.join("libGLESv2.so.2"), b"\x7fELF-real-gles").unwrap();

        // The overlay upper as a legacy image would leave it:
        //   0-byte inject stubs that MUST be pruned (they shadow the guest's real libs -> ENOEXEC)…
        for stub in ["libz.so.1", "libffi.so.8", "libstdc++.so.6", "libgcc_s.so.1"] {
            std::fs::write(overlay.join(stub), b"").unwrap();
        }
        // …a 0-byte stub for a soname we DO bind a real lib over this run: harmless (covered), left as-is…
        std::fs::write(overlay.join("libEGL.so.1"), b"").unwrap();
        // …a REAL (non-empty) distro lib the guest legitimately shipped: MUST be untouched…
        std::fs::write(overlay.join("libX11.so.6"), b"\x7fELF-real-x11").unwrap();
        // …and a non-.so 0-byte file: not an inject target, MUST be untouched.
        std::fs::write(overlay.join("keep.conf"), b"").unwrap();

        let g = GpuIntegration::new(GuestArch::Aarch64).with_display(DisplayIntegration {
            wayland_sock: "/w".into(),
            gpu_exec_sock: "/e".into(),
            lib_dir: dropin.to_string_lossy().into_owned(),
            bin_dir: String::new(),
            overlay_lib_dir: overlay.to_string_lossy().into_owned(),
        });
        let req = g.device_request(&[]);

        // The render shims are injected over the guest multiarch dir…
        let bound: Vec<&str> = req.mounts.iter().map(|m| m.container.as_str()).collect();
        assert!(bound.contains(&"/usr/lib/aarch64-linux-gnu/libEGL.so.1"), "libEGL must be injected: {bound:?}");
        assert!(bound.contains(&"/usr/lib/aarch64-linux-gnu/libGLESv2.so.2"), "libGLESv2 must be injected: {bound:?}");

        // …the orphaned 0-byte stubs are GONE from the overlay (guest now resolves its own base libs)…
        for pruned in ["libz.so.1", "libffi.so.8", "libstdc++.so.6", "libgcc_s.so.1"] {
            assert!(!overlay.join(pruned).exists(), "stale 0-byte stub {pruned} must be pruned");
        }
        // …the real distro lib survives (never pruned — a real lib is never 0 bytes)…
        assert!(overlay.join("libX11.so.6").exists(), "real distro libX11 must be untouched");
        let x11 = std::fs::read(overlay.join("libX11.so.6")).unwrap();
        assert_eq!(x11, b"\x7fELF-real-x11", "real distro libX11 content must be intact");
        // …the 0-byte stub for a bound soname is left alone (the bind already covers it)…
        assert!(overlay.join("libEGL.so.1").exists(), "a bound soname's stub must not be pruned (it's covered)");
        // …and the non-.so 0-byte file is untouched.
        assert!(overlay.join("keep.conf").exists(), "non-.so file must be untouched");

        // Direct-unit assertion on the helper's return value: exactly the four orphans, sorted.
        let mut bound_set = std::collections::BTreeSet::new();
        bound_set.insert("libEGL.so.1".to_string());
        bound_set.insert("libGLESv2.so.2".to_string());
        // Re-seed the overlay to re-run the helper in isolation.
        for stub in ["libz.so.1", "libffi.so.8", "libstdc++.so.6", "libgcc_s.so.1"] {
            std::fs::write(overlay.join(stub), b"").unwrap();
        }
        let pruned = prune_shadowing_stubs(&overlay.to_string_lossy(), &bound_set);
        assert_eq!(pruned, vec!["libffi.so.8", "libgcc_s.so.1", "libstdc++.so.6", "libz.so.1"]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prune_removes_stale_mismatched_abi_render_shim() {
        // The GTK-after-Chrome failure: a PRIOR musl launch left a REAL (non-empty) musl
        // `libwayland-egl.so.1` in the overlay upper; this glibc run does NOT bind that soname (say the
        // glibc variant lacks it, or it simply isn't in the drop-in), so the stale musl shim would shadow
        // the guest's real glibc libwayland-egl on LD_LIBRARY_PATH -> `libc.musl-…: cannot open`. A
        // shim-owned lib in the UPPER we aren't binding this run must be pruned regardless of size.
        let base = std::env::temp_dir().join(format!("ddgpu-abiprune-{}", std::process::id()));
        let overlay = base.join("overlay");
        std::fs::create_dir_all(&overlay).unwrap();

        // A real (non-empty) stale musl render shim we are NOT binding this run -> must be pruned.
        std::fs::write(overlay.join("libwayland-egl.so.1"), b"\x7fELF-stale-musl-wl-egl").unwrap();
        // A render shim we ARE binding this run -> covered by the mount, left as-is even if non-empty.
        std::fs::write(overlay.join("libEGL.so.1"), b"\x7fELF-old-egl").unwrap();
        // A real distro lib the shim does NOT own -> never pruned, whatever its size.
        std::fs::write(overlay.join("libX11.so.6"), b"\x7fELF-real-x11").unwrap();
        // A 0-byte non-shim stub -> still pruned (the original behavior).
        std::fs::write(overlay.join("libffi.so.8"), b"").unwrap();

        let mut bound = std::collections::BTreeSet::new();
        bound.insert("libEGL.so.1".to_string());
        let pruned = prune_shadowing_stubs(&overlay.to_string_lossy(), &bound);

        assert_eq!(pruned, vec!["libffi.so.8", "libwayland-egl.so.1"], "prune the stale mismatched shim + 0-byte stub only");
        assert!(!overlay.join("libwayland-egl.so.1").exists(), "stale mismatched-ABI render shim must be pruned");
        assert!(overlay.join("libEGL.so.1").exists(), "a shim bound this run must NOT be pruned (mount covers it)");
        assert!(overlay.join("libX11.so.6").exists(), "a non-shim distro lib must never be pruned");

        let _ = std::fs::remove_dir_all(&base);
    }
}
