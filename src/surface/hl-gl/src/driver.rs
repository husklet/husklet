//! The **driver-plugin seam**: [`Gl`] implements [`hl_jit::Driver`] so a container launch attaches the
//! GLES/EGL backend generically — `engine.add(Gl::new(spec))` (mirrors [`hl_cuda`'s `Cuda`]). This is the
//! HOST composition-root side; it holds ALL GL-specific knowledge and hands the runtime only a
//! runtime-neutral [`hl_jit::DeviceRequest`] (bind mounts + env), never any GPU command semantics.
//!
//! [`Gl::device_request`] injects the staged guest shim `.so`s at their guest sonames (`libEGL.so.1` +
//! the `libGLESv2.so.2` DT_NEEDED→libEGL forwarding stub), prepends the shim libdir to
//! `LD_LIBRARY_PATH`, binds the `$HL_GPU_EXEC` socket, and sets the optional `HL_GL_*` surface-size env
//! the guest shim reads. The guest shim then speaks the neutral `hl_gpu` command protocol to the host
//! GPU-exec service over that socket.
//!
//! Gated behind the `jit` cargo feature (default-on for this crate's own build/tests) so the guest shim
//! cdylib — which depends on this crate only for the lowering services — never drags hl-jit/tokio into a
//! guest `.so`.

use std::path::{Path, PathBuf};

use hl_jit::{DeviceMount, DeviceRequest, Driver};

/// A guest CPU architecture the shims are staged for. Selects both the staged-artifact directory
/// (`~/.hl/gl/<dir>/`) and the guest multiarch library directory the shims mount into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    Aarch64,
    X86_64,
}

impl Arch {
    /// The `~/.hl/gl/<dir>/` staging subdirectory name.
    pub fn dir(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
        }
    }

    /// The guest multiarch library directory the shim sonames are bound into (Debian/Ubuntu layout).
    pub fn guest_libdir(self) -> &'static str {
        match self {
            Arch::Aarch64 => "/usr/lib/aarch64-linux-gnu",
            Arch::X86_64 => "/usr/lib/x86_64-linux-gnu",
        }
    }
}

/// How to attach the GL backend to a launch: which guest arch, where the host GPU-exec socket lives, and
/// the surface size to advertise. Build it with [`GlSpec::new`] and override the optional fields.
#[derive(Clone, Debug)]
pub struct GlSpec {
    /// The guest's CPU architecture (selects the staged `.so` set + guest libdir).
    pub arch: Arch,
    /// Host path to the `$HL_GPU_EXEC` Unix socket the host GPU-exec service listens on.
    pub exec_socket: PathBuf,
    /// Guest path the socket is bind-mounted at (and what `$HL_GPU_EXEC` is set to inside the guest).
    pub guest_socket: String,
    /// Staging root the shim cross-compile installs under (`build.rs` stages to `<root>/gl/<arch>/…`).
    /// Defaults to `~/.hl`.
    pub stage_root: PathBuf,
    /// Advertised default window-surface size `(width, height)` (`$HL_GL_SURFACE_W` / `_H`). `None` = the
    /// shim's built-in default (1280x720).
    pub surface_size: Option<(u32, u32)>,
}

impl GlSpec {
    /// A spec for `arch` whose host GPU-exec socket is at `exec_socket`, with defaults: socket mounted at
    /// `/run/hl-gpu.sock`, staging root `~/.hl`, and the shim's built-in surface size.
    pub fn new(arch: Arch, exec_socket: impl Into<PathBuf>) -> Self {
        GlSpec {
            arch,
            exec_socket: exec_socket.into(),
            guest_socket: "/run/hl-gpu.sock".to_string(),
            stage_root: default_stage_root(),
            surface_size: None,
        }
    }

    /// Override the guest socket mount path (and `$HL_GPU_EXEC` value).
    pub fn guest_socket(mut self, path: impl Into<String>) -> Self {
        self.guest_socket = path.into();
        self
    }

    /// Override the staging root the shim `.so`s are read from.
    pub fn stage_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.stage_root = root.into();
        self
    }

    /// Advertise a default window-surface size to the guest (`HL_GL_SURFACE_W` / `_H`).
    pub fn surface_size(mut self, width: u32, height: u32) -> Self {
        self.surface_size = Some((width, height));
        self
    }
}

/// `~/.hl` (or `/root/.hl` if `$HOME` is unset) — where the shim cross-compile stages its artifacts.
fn default_stage_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    Path::new(&home).join(".hl")
}

/// The `hl_jit::Driver` plug for the GL backend. `engine.add(Gl::new(spec))` attaches it to a launch.
pub struct Gl {
    spec: GlSpec,
}

impl Gl {
    /// Build the driver from a [`GlSpec`].
    pub fn new(spec: GlSpec) -> Self {
        Gl { spec }
    }

    /// Host path of a staged guest soname under `<stage_root>/gl/<arch>/<soname>`.
    fn staged(&self, soname: &str) -> PathBuf {
        self.spec
            .stage_root
            .join("gl")
            .join(self.spec.arch.dir())
            .join(soname)
    }

    /// Prepend this driver's guest library directory to `LD_LIBRARY_PATH`.
    fn library_path(&self, guest_env: &[String]) -> String {
        let directory = self.spec.arch.guest_libdir();
        let existing = guest_env
            .iter()
            .find_map(|value| value.strip_prefix("LD_LIBRARY_PATH="))
            .filter(|value| !value.is_empty());
        match existing {
            Some(value) => format!("LD_LIBRARY_PATH={directory}:{value}"),
            None => format!("LD_LIBRARY_PATH={directory}"),
        }
    }
}

impl Driver for Gl {
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest {
        let libdir = self.spec.arch.guest_libdir();

        // 1. Bind the staged guest shim .so's at both their ABI sonames and linker names. ELF dependencies
        //    use the versioned names, while driver loaders such as Chromium's ANGLE deliberately dlopen
        //    `libEGL.so` / `libGLESv2.so`. Both forms must select the same injected implementation.
        let mut mounts = vec![
            DeviceMount::ro(
                self.staged("libEGL.so.1").to_string_lossy().into_owned(),
                format!("{libdir}/libEGL.so.1"),
            ),
            DeviceMount::ro(
                self.staged("libEGL.so.1").to_string_lossy().into_owned(),
                format!("{libdir}/libEGL.so"),
            ),
            DeviceMount::ro(
                self.staged("libGLESv2.so.2").to_string_lossy().into_owned(),
                format!("{libdir}/libGLESv2.so.2"),
            ),
            DeviceMount::ro(
                self.staged("libGLESv2.so.2").to_string_lossy().into_owned(),
                format!("{libdir}/libGLESv2.so"),
            ),
        ];
        // 2. Bind the host GPU-exec socket (read-write: the guest connects to it).
        mounts.push(DeviceMount::rw(
            self.spec.exec_socket.to_string_lossy().into_owned(),
            self.spec.guest_socket.clone(),
        ));

        // 3. Env: prepend the shim libdir to LD_LIBRARY_PATH, name the exec socket, advertise the surface.
        let mut env = vec![
            self.library_path(guest_env),
            format!("HL_GPU_EXEC={}", self.spec.guest_socket),
        ];
        if let Some((w, h)) = self.spec.surface_size {
            env.push(format!("HL_GL_SURFACE_W={w}"));
            env.push(format!("HL_GL_SURFACE_H={h}"));
        }

        // Injecting shim libraries + a socket; no synthetic render node.
        DeviceRequest {
            mounts,
            env,
            render_node: false,
        }
    }

    fn name(&self) -> &str {
        "gl"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> GlSpec {
        GlSpec::new(Arch::Aarch64, "/tmp/hl-gpu.sock")
            .stage_root("/opt/hlroot")
            .surface_size(1920, 1080)
    }

    #[test]
    fn device_request_binds_the_two_sonames_and_socket() {
        let gl = Gl::new(spec());
        let req = gl.device_request(&[]);
        assert_eq!(gl.name(), "gl");
        assert!(!req.render_node);

        let libdir = "/usr/lib/aarch64-linux-gnu";
        let want = [
            (
                "/opt/hlroot/gl/aarch64/libEGL.so.1",
                format!("{libdir}/libEGL.so.1"),
                true,
            ),
            (
                "/opt/hlroot/gl/aarch64/libEGL.so.1",
                format!("{libdir}/libEGL.so"),
                true,
            ),
            (
                "/opt/hlroot/gl/aarch64/libGLESv2.so.2",
                format!("{libdir}/libGLESv2.so.2"),
                true,
            ),
            (
                "/opt/hlroot/gl/aarch64/libGLESv2.so.2",
                format!("{libdir}/libGLESv2.so"),
                true,
            ),
        ];
        for (host, container, ro) in want {
            assert!(
                req.mounts
                    .iter()
                    .any(|m| m.host == host && m.container == container && m.read_only == ro),
                "missing bind {host} -> {container}"
            );
        }
        // The exec socket is a read-write bind.
        assert!(req.mounts.iter().any(|m| m.host == "/tmp/hl-gpu.sock"
            && m.container == "/run/hl-gpu.sock"
            && !m.read_only));
    }

    #[test]
    fn env_prepends_ld_library_path_and_advertises_surface() {
        let req = Gl::new(spec()).device_request(&["LD_LIBRARY_PATH=/usr/lib:/lib".to_string()]);
        assert!(req
            .env
            .contains(&"LD_LIBRARY_PATH=/usr/lib/aarch64-linux-gnu:/usr/lib:/lib".to_string()));
        assert!(req
            .env
            .contains(&"HL_GPU_EXEC=/run/hl-gpu.sock".to_string()));
        assert!(req.env.contains(&"HL_GL_SURFACE_W=1920".to_string()));
        assert!(req.env.contains(&"HL_GL_SURFACE_H=1080".to_string()));
    }

    #[test]
    fn ld_library_path_is_created_when_absent() {
        let req = Gl::new(GlSpec::new(Arch::X86_64, "/s.sock")).device_request(&[]);
        assert!(req
            .env
            .contains(&"LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu".to_string()));
    }
}
