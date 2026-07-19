//! The **driver-plugin seam**: [`Vulkan`] implements [`hl_jit::Driver`] so a container launch attaches
//! the Vulkan backend generically — `engine.add(Vulkan::new(spec))` (goal.md OVERVIEW §4). This is the
//! HOST composition-root side; it holds ALL Vulkan-specific knowledge and hands the runtime only a
//! runtime-neutral [`hl_jit::DeviceRequest`] (bind mounts + env), never any GPU command semantics.
//!
//! [`Vulkan::device_request`] injects the staged guest ICD `libvk_hl.so.1` into the guest multiarch
//! libdir (plus the unversioned `libvk_hl.so` the `icd.json` `library_path` names), drops the driver
//! `icd.json` and points the Vulkan loader at it with `VK_ICD_FILENAMES`, prepends the shim libdir to
//! `LD_LIBRARY_PATH`, and binds the `$HL_GPU_EXEC` socket. The guest ICD then speaks the neutral
//! `hl_gpu` command protocol to the host GPU-exec service over that socket.
//!
//! Gated behind the `jit` cargo feature (default-on for this crate's own build/tests) so the guest ICD
//! cdylib — which depends on this crate only for the lowering services — never drags hl-jit/tokio into a
//! guest `.so`.

use std::path::{Path, PathBuf};

use hl_jit::{DeviceMount, DeviceRequest, Driver};

/// A guest CPU architecture the ICD is staged for. Selects both the staged-artifact directory
/// (`~/.hl/vulkan/<dir>/`) and the guest multiarch library directory the ICD mounts into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    Aarch64,
    X86_64,
}

impl Arch {
    /// The `~/.hl/vulkan/<dir>/` staging subdirectory name.
    pub fn dir(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
        }
    }

    /// The guest multiarch library directory the ICD is bound into (Debian/Ubuntu layout). The
    /// `icd.json` is dropped here too, so its relative `library_path: ./libvk_hl.so` resolves.
    pub fn guest_libdir(self) -> &'static str {
        match self {
            Arch::Aarch64 => "/usr/lib/aarch64-linux-gnu",
            Arch::X86_64 => "/usr/lib/x86_64-linux-gnu",
        }
    }
}

/// How to attach the Vulkan backend to a launch: which guest arch, where the host GPU-exec socket lives,
/// and where to stage from. Build it with [`VulkanSpec::new`] and override the optional fields.
#[derive(Clone, Debug)]
pub struct VulkanSpec {
    /// The guest's CPU architecture (selects the staged ICD set + guest libdir).
    pub arch: Arch,
    /// Host path to the `$HL_GPU_EXEC` Unix socket the host GPU-exec service listens on.
    pub exec_socket: PathBuf,
    /// Guest path the socket is bind-mounted at (and what `$HL_GPU_EXEC` is set to inside the guest).
    pub guest_socket: String,
    /// Staging root the ICD cross-compile installs under (`build.rs` stages to
    /// `<root>/vulkan/<arch>/…`). Defaults to `~/.hl`.
    pub stage_root: PathBuf,
    /// The guest filename the driver `icd.json` is dropped as (inside the guest libdir). The Vulkan
    /// loader is pointed at it via `VK_ICD_FILENAMES`.
    pub icd_manifest_name: String,
}

impl VulkanSpec {
    /// A spec for `arch` whose host GPU-exec socket is at `exec_socket`, with defaults: socket mounted
    /// at `/run/hl-gpu.sock`, staging root `~/.hl`, and the `icd.json` dropped as `hl_vulkan_icd.json`.
    pub fn new(arch: Arch, exec_socket: impl Into<PathBuf>) -> Self {
        VulkanSpec {
            arch,
            exec_socket: exec_socket.into(),
            guest_socket: "/run/hl-gpu.sock".to_string(),
            stage_root: default_stage_root(),
            icd_manifest_name: "hl_vulkan_icd.json".to_string(),
        }
    }

    /// Override the guest socket mount path (and `$HL_GPU_EXEC` value).
    pub fn guest_socket(mut self, path: impl Into<String>) -> Self {
        self.guest_socket = path.into();
        self
    }

    /// Override the staging root the ICD `.so` + `icd.json` are read from.
    pub fn stage_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.stage_root = root.into();
        self
    }
}

/// `~/.hl` (or `/root/.hl` if `$HOME` is unset) — where the ICD cross-compile stages its artifacts.
fn default_stage_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    Path::new(&home).join(".hl")
}

/// The `hl_jit::Driver` plug for the Vulkan backend. `engine.add(Vulkan::new(spec))` attaches it to a
/// launch.
pub struct Vulkan {
    spec: VulkanSpec,
}

impl Vulkan {
    /// Build the driver from a [`VulkanSpec`].
    pub fn new(spec: VulkanSpec) -> Self {
        Vulkan { spec }
    }

    /// Host path of a staged artifact under `<stage_root>/vulkan/<arch>/<name>`.
    fn staged(&self, name: &str) -> PathBuf {
        self.spec
            .stage_root
            .join("vulkan")
            .join(self.spec.arch.dir())
            .join(name)
    }

    /// Prepend `dir` to the guest environment's existing library path.
    fn library_path(&self, guest_env: &[String], dir: &str) -> String {
        let existing = guest_env
            .iter()
            .find_map(|value| value.strip_prefix("LD_LIBRARY_PATH="))
            .filter(|value| !value.is_empty());
        match existing {
            Some(value) => format!("LD_LIBRARY_PATH={dir}:{value}"),
            None => format!("LD_LIBRARY_PATH={dir}"),
        }
    }
}

impl Driver for Vulkan {
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest {
        let libdir = self.spec.arch.guest_libdir();
        let icd_path = format!("{libdir}/{}", self.spec.icd_manifest_name);
        let staged_so = self.staged("libvk_hl.so.1").to_string_lossy().into_owned();

        // 1. Bind the staged guest ICD at its versioned soname AND the unversioned name the icd.json's
        //    `library_path: ./libvk_hl.so` resolves to (both in the guest multiarch libdir).
        let mounts = vec![
            DeviceMount::ro(staged_so.clone(), format!("{libdir}/libvk_hl.so.1")),
            DeviceMount::ro(staged_so, format!("{libdir}/libvk_hl.so")),
            // 2. Drop the driver icd.json in the same libdir so `./libvk_hl.so` resolves next to it.
            DeviceMount::ro(
                self.staged("icd.json").to_string_lossy().into_owned(),
                icd_path.clone(),
            ),
            // 3. Bind the host GPU-exec socket (read-write: the guest connects to it).
            DeviceMount::rw(
                self.spec.exec_socket.to_string_lossy().into_owned(),
                self.spec.guest_socket.clone(),
            ),
        ];

        // 4. Env: prepend the shim libdir to LD_LIBRARY_PATH, point the Vulkan loader at the driver
        //    icd.json (VK_ICD_FILENAMES), and name the exec socket.
        let env = vec![
            self.library_path(guest_env, libdir),
            format!("VK_ICD_FILENAMES={icd_path}"),
            format!("HL_GPU_EXEC={}", self.spec.guest_socket),
        ];

        // Injecting an ICD library + its manifest + a socket; no synthetic render node.
        DeviceRequest {
            mounts,
            env,
            render_node: false,
        }
    }

    fn name(&self) -> &str {
        "vulkan"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VulkanSpec {
        VulkanSpec::new(Arch::Aarch64, "/tmp/hl-gpu.sock").stage_root("/opt/hlroot")
    }

    #[test]
    fn device_request_binds_the_icd_manifest_and_socket() {
        let vk = Vulkan::new(spec());
        let req = vk.device_request(&[]);
        assert_eq!(vk.name(), "vulkan");
        assert!(!req.render_node);

        let libdir = "/usr/lib/aarch64-linux-gnu";
        // The ICD is bound at both its versioned + unversioned names, plus its manifest.
        let want = [
            (
                "/opt/hlroot/vulkan/aarch64/libvk_hl.so.1",
                format!("{libdir}/libvk_hl.so.1"),
            ),
            (
                "/opt/hlroot/vulkan/aarch64/libvk_hl.so.1",
                format!("{libdir}/libvk_hl.so"),
            ),
            (
                "/opt/hlroot/vulkan/aarch64/icd.json",
                format!("{libdir}/hl_vulkan_icd.json"),
            ),
        ];
        for (host, container) in want {
            assert!(
                req.mounts
                    .iter()
                    .any(|m| m.host == host && m.container == container && m.read_only),
                "missing ro bind {host} -> {container}"
            );
        }
        // The exec socket is a read-write bind.
        assert!(req.mounts.iter().any(|m| m.host == "/tmp/hl-gpu.sock"
            && m.container == "/run/hl-gpu.sock"
            && !m.read_only));
    }

    #[test]
    fn env_points_loader_at_the_icd_and_prepends_ld_library_path() {
        let req =
            Vulkan::new(spec()).device_request(&["LD_LIBRARY_PATH=/usr/lib:/lib".to_string()]);
        assert!(req
            .env
            .contains(&"LD_LIBRARY_PATH=/usr/lib/aarch64-linux-gnu:/usr/lib:/lib".to_string()));
        assert!(req.env.contains(
            &"VK_ICD_FILENAMES=/usr/lib/aarch64-linux-gnu/hl_vulkan_icd.json".to_string()
        ));
        assert!(req
            .env
            .contains(&"HL_GPU_EXEC=/run/hl-gpu.sock".to_string()));
    }

    #[test]
    fn ld_library_path_is_created_when_absent() {
        let req = Vulkan::new(VulkanSpec::new(Arch::X86_64, "/s.sock")).device_request(&[]);
        assert!(req
            .env
            .contains(&"LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu".to_string()));
    }
}
