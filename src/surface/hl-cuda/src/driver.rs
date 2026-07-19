//! The **driver-plugin seam**: [`Cuda`] implements [`hl_jit::Driver`] so a container launch attaches the
//! CUDA backend generically — `engine.add(Cuda::new(spec))` (goal.md OVERVIEW §4). This is the HOST
//! composition-root side; it holds ALL CUDA-specific knowledge and hands the runtime only a
//! runtime-neutral [`hl_jit::DeviceRequest`] (bind mounts + env), never any GPU command semantics.
//!
//! [`Cuda::device_request`] injects the three staged guest shim `.so`s at their guest sonames, prepends
//! the shim libdir to `LD_LIBRARY_PATH`, binds the `$HL_GPU_EXEC` socket, and sets the `HL_CUDA_*`
//! device-advertisement env the guest shims read. The guest shims then speak the neutral
//! `hl_gpu` command protocol to the host GPU-exec service over that socket.
//!
//! Gated behind the `jit` cargo feature (default-on for this crate's own build/tests) so the guest shim
//! cdylibs — which depend on this crate only for the lowering services — never drag hl-jit/tokio into a
//! guest `.so`.

use std::path::{Path, PathBuf};

use hl_jit::{DeviceMount, DeviceRequest, Driver};

/// A guest CPU architecture the shims are staged for. Selects both the staged-artifact directory
/// (`~/.hl/cuda/<dir>/`) and the guest multiarch library directory the shims mount into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    Aarch64,
    X86_64,
}

impl Arch {
    /// The `~/.hl/{cuda,nvml}/<dir>/` staging subdirectory name.
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

/// How to attach the CUDA backend to a launch: which guest arch, where the host GPU-exec socket lives,
/// and the device numbers to advertise. Build it with [`CudaSpec::new`] and override the optional fields.
#[derive(Clone, Debug)]
pub struct CudaSpec {
    /// The guest's CPU architecture (selects the staged `.so` set + guest libdir).
    pub arch: Arch,
    /// Host path to the `$HL_GPU_EXEC` Unix socket the host GPU-exec service listens on.
    pub exec_socket: PathBuf,
    /// Guest path the socket is bind-mounted at (and what `$HL_GPU_EXEC` is set to inside the guest).
    pub guest_socket: String,
    /// Staging root the shim cross-compile installs under (`build.rs` stages to `<root>/cuda/<arch>/…`
    /// and `<root>/nvml/<arch>/…`). Defaults to `~/.hl`.
    pub stage_root: PathBuf,
    /// Advertised device name (`$HL_CUDA_NAME`). `None` = the shim's built-in default.
    pub device_name: Option<String>,
    /// Advertised compute capability as `"major.minor"` (`$HL_CUDA_CC`).
    pub compute_capability: Option<String>,
    /// Advertised VRAM in bytes (`$HL_CUDA_VRAM_BYTES`).
    pub vram_bytes: Option<u64>,
}

impl CudaSpec {
    /// A spec for `arch` whose host GPU-exec socket is at `exec_socket`, with defaults: socket mounted at
    /// `/run/hl-gpu.sock`, staging root `~/.hl`, and the shim's built-in device numbers.
    pub fn new(arch: Arch, exec_socket: impl Into<PathBuf>) -> Self {
        CudaSpec {
            arch,
            exec_socket: exec_socket.into(),
            guest_socket: "/run/hl-gpu.sock".to_string(),
            stage_root: default_stage_root(),
            device_name: None,
            compute_capability: None,
            vram_bytes: None,
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

    /// Advertise a device name / compute capability / VRAM to the guest (`HL_CUDA_*`).
    pub fn advertise(
        mut self,
        name: impl Into<String>,
        compute_capability: impl Into<String>,
        vram_bytes: u64,
    ) -> Self {
        self.device_name = Some(name.into());
        self.compute_capability = Some(compute_capability.into());
        self.vram_bytes = Some(vram_bytes);
        self
    }
}

/// `~/.hl` (or `/root/.hl` if `$HOME` is unset) — where the shim cross-compile stages its artifacts.
fn default_stage_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    Path::new(&home).join(".hl")
}

/// The `hl_jit::Driver` plug for the CUDA backend. `engine.add(Cuda::new(spec))` attaches it to a launch.
pub struct Cuda {
    spec: CudaSpec,
}

impl Cuda {
    /// Build the driver from a [`CudaSpec`].
    pub fn new(spec: CudaSpec) -> Self {
        Cuda { spec }
    }

    /// Host path of a staged guest soname under `<stage_root>/<family>/<arch>/<soname>`.
    fn staged(&self, family: &str, soname: &str) -> PathBuf {
        self.spec
            .stage_root
            .join(family)
            .join(self.spec.arch.dir())
            .join(soname)
    }
}

/// Prepend `dir` to the guest env's existing `LD_LIBRARY_PATH` (if any), producing the new `K=V` line.
struct GuestEnvironment<'a>(&'a [String]);

impl GuestEnvironment<'_> {
    fn library_path(&self, dir: &str) -> String {
        let existing = self
            .0
            .iter()
            .find_map(|kv| kv.strip_prefix("LD_LIBRARY_PATH="))
            .filter(|v| !v.is_empty());
        match existing {
            Some(v) => format!("LD_LIBRARY_PATH={dir}:{v}"),
            None => format!("LD_LIBRARY_PATH={dir}"),
        }
    }
}

impl Driver for Cuda {
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest {
        let libdir = self.spec.arch.guest_libdir();

        // 1. Bind the three staged guest shim .so's at their guest sonames in the guest multiarch libdir,
        //    so a guest app that DT_NEEDEDs libcuda.so.1 / libcudart.so.1 / libnvidia-ml.so.1 resolves them.
        let mut mounts = vec![
            DeviceMount::ro(
                self.staged("cuda", "libcuda.so.1")
                    .to_string_lossy()
                    .into_owned(),
                format!("{libdir}/libcuda.so.1"),
            ),
            DeviceMount::ro(
                self.staged("cuda", "libcudart.so.1")
                    .to_string_lossy()
                    .into_owned(),
                format!("{libdir}/libcudart.so.1"),
            ),
            DeviceMount::ro(
                self.staged("nvml", "libnvidia-ml.so.1")
                    .to_string_lossy()
                    .into_owned(),
                format!("{libdir}/libnvidia-ml.so.1"),
            ),
        ];
        // 2. Bind the host GPU-exec socket (read-write: the guest connects to it).
        mounts.push(DeviceMount::rw(
            self.spec.exec_socket.to_string_lossy().into_owned(),
            self.spec.guest_socket.clone(),
        ));

        // 3. Env: prepend the shim libdir to LD_LIBRARY_PATH, name the exec socket, advertise the device.
        let mut env = vec![
            GuestEnvironment(guest_env).library_path(libdir),
            format!("HL_GPU_EXEC={}", self.spec.guest_socket),
        ];
        if let Some(name) = &self.spec.device_name {
            env.push(format!("HL_CUDA_NAME={name}"));
        }
        if let Some(cc) = &self.spec.compute_capability {
            env.push(format!("HL_CUDA_CC={cc}"));
        }
        if let Some(vram) = self.spec.vram_bytes {
            env.push(format!("HL_CUDA_VRAM_BYTES={vram}"));
        }

        // Injecting shim libraries + a socket; no synthetic render node.
        DeviceRequest {
            mounts,
            env,
            render_node: false,
        }
    }

    fn name(&self) -> &str {
        "cuda"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CudaSpec {
        CudaSpec::new(Arch::Aarch64, "/tmp/hl-gpu.sock")
            .stage_root("/opt/hlroot")
            .advertise("Tesla hl-Metal", "8.6", 16 << 30)
    }

    #[test]
    fn device_request_binds_the_three_sonames_and_socket() {
        let cuda = Cuda::new(spec());
        let req = cuda.device_request(&[]);
        assert_eq!(cuda.name(), "cuda");
        assert!(!req.render_node);

        // The three shim sonames, staged host path -> guest multiarch libdir.
        let libdir = "/usr/lib/aarch64-linux-gnu";
        let want = [
            (
                "/opt/hlroot/cuda/aarch64/libcuda.so.1",
                format!("{libdir}/libcuda.so.1"),
                true,
            ),
            (
                "/opt/hlroot/cuda/aarch64/libcudart.so.1",
                format!("{libdir}/libcudart.so.1"),
                true,
            ),
            (
                "/opt/hlroot/nvml/aarch64/libnvidia-ml.so.1",
                format!("{libdir}/libnvidia-ml.so.1"),
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
    fn env_prepends_ld_library_path_and_advertises_device() {
        let req = Cuda::new(spec()).device_request(&["LD_LIBRARY_PATH=/usr/lib:/lib".to_string()]);
        assert!(req
            .env
            .contains(&"LD_LIBRARY_PATH=/usr/lib/aarch64-linux-gnu:/usr/lib:/lib".to_string()));
        assert!(req
            .env
            .contains(&"HL_GPU_EXEC=/run/hl-gpu.sock".to_string()));
        assert!(req.env.contains(&"HL_CUDA_NAME=Tesla hl-Metal".to_string()));
        assert!(req.env.contains(&"HL_CUDA_CC=8.6".to_string()));
        assert!(req
            .env
            .contains(&"HL_CUDA_VRAM_BYTES=17179869184".to_string()));
    }

    #[test]
    fn ld_library_path_is_created_when_absent() {
        let req = Cuda::new(CudaSpec::new(Arch::X86_64, "/s.sock")).device_request(&[]);
        assert!(req
            .env
            .contains(&"LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu".to_string()));
    }
}
