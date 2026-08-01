//! Guest projection of Husklet's graphics drivers.
//!
//! The drivers live in a Husklet-owned directory placed first on the guest loader search path,
//! never over the distribution's multiarch library paths. Binding a read-only host file onto
//! `/usr/lib/<multiarch>/libgbm.so.1` makes that path `EROFS`, so `dpkg` cannot rename its
//! `.dpkg-new` staging file over it and every ordinary GL package fails to unpack. Keeping the
//! multiarch directory entirely unclaimed leaves it owned by the guest package manager, while
//! `LD_LIBRARY_PATH` still resolves GL, GBM and Vulkan to Husklet's drivers: glibc searches
//! `LD_LIBRARY_PATH` ahead of `DT_RUNPATH`, `ld.so.cache` and the default directories, for
//! `DT_NEEDED` dependencies and for `dlopen` by soname alike.

use hl_container::device::extension::{
    BindAccess, FileEntry, FileSource, HostBindEntry, Metadata, NamespaceEntry,
};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Guest-visible layout of the projected driver set for one architecture.
pub(super) struct Projection {
    arch: hl_ws::Arch,
}

impl Projection {
    pub(super) const fn new(arch: hl_ws::Arch) -> Self {
        Self { arch }
    }

    /// Husklet-owned guest directory holding every projected driver.
    ///
    /// It sits beside the multiarch directory rather than inside `/opt` so that a sandboxed
    /// process keeps the same filesystem policy surface it already has for `/usr/lib`.
    ///
    /// The directory itself is never projected. Each bind materializes its own mount point, and a
    /// projected directory would instead become a mount of its own whose (empty) backing tree is
    /// what `readdir` reports -- the drivers stay openable by exact path, but the directory lists
    /// as empty and nothing that enumerates the search path can see them.
    pub(super) const fn directory(&self) -> &'static str {
        match self.arch {
            hl_ws::Arch::Arm64 => "/usr/lib/aarch64-linux-gnu/husklet",
            hl_ws::Arch::Amd64 => "/usr/lib/x86_64-linux-gnu/husklet",
        }
    }

    pub(super) const fn vulkan_manifest(&self) -> &'static str {
        match self.arch {
            // Chromium's Linux GPU broker permits only a fixed set of distribution manifest
            // basenames after sandbox activation. Manifest names carry no Vulkan vendor
            // semantics; the JSON library path remains the authoritative driver identity.
            hl_ws::Arch::Arm64 => "freedreno_icd.aarch64.json",
            hl_ws::Arch::Amd64 => "intel_icd.x86_64.json",
        }
    }

    pub(super) const fn vulkan_library(&self) -> &'static str {
        match self.arch {
            hl_ws::Arch::Arm64 => "libvulkan_freedreno.so",
            hl_ws::Arch::Amd64 => "libvulkan_intel.so",
        }
    }

    /// Packaged driver, followed by every soname the guest loader may request for it.
    pub(super) fn bindings() -> [(&'static str, &'static str, &'static str); 9] {
        [
            ("gl", "libEGL.so.1", "libEGL.so.1"),
            ("gl", "libEGL.so.1", "libEGL.so"),
            ("gl", "libGLESv2.so.2", "libGLESv2.so.2"),
            ("gl", "libGLESv2.so.2", "libGLESv2.so"),
            ("gl", "libgbm.so.1", "libgbm.so.1"),
            ("gl", "libgbm.so.1", "libgbm.so.1.0.0"),
            ("gl", "libgbm.so.1", "libgbm.so"),
            ("vulkan", "libvk_hl.so.1", "libvk_hl.so.1"),
            ("vulkan", "libvk_hl.so.1", "libvk_hl.so"),
        ]
    }

    pub(super) fn compute() -> [(&'static str, &'static str); 3] {
        [
            ("cuda", "libcuda.so.1"),
            ("cuda", "libcudart.so.1"),
            ("nvml", "libnvidia-ml.so.1"),
        ]
    }

    pub(super) fn guest_environment() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
            ("XDG_RUNTIME_DIR".to_owned(), "/run".to_owned()),
            ("XDG_SESSION_TYPE".to_owned(), "wayland".to_owned()),
        ])
    }

    /// Read-only bind of one packaged driver into the Husklet driver directory.
    fn bind(&self, host: PathBuf, name: &str) -> NamespaceEntry {
        NamespaceEntry::HostBind(HostBindEntry {
            path: Path::new(self.directory()).join(name),
            host,
            access: BindAccess::ReadOnly,
        })
    }

    /// Graphics driver files plus the Vulkan manifest that names one of them.
    pub(super) fn graphics(
        &self,
        drivers: &crate::runtime::drivers::Drivers,
    ) -> io::Result<Vec<NamespaceEntry>> {
        let mut namespace = Self::bindings()
            .into_iter()
            .map(|(family, source, target)| self.bind(drivers.path(family, source), target))
            .collect::<Vec<_>>();
        let source = fs::read_to_string(drivers.path("vulkan", "icd.json"))?;
        let relative = "\"./libvk_hl.so\"";
        if !source.contains(relative) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Vulkan manifest does not name the packaged driver",
            ));
        }
        let library = Path::new(self.directory()).join(self.vulkan_library());
        // An absolute library path keeps the Vulkan loader independent of the search path, which a
        // sandboxed process may not share with the launcher.
        let contents = source.replace(relative, &format!("\"{}\"", library.display()));
        namespace.push(self.bind(
            drivers.path("vulkan", "libvk_hl.so.1"),
            self.vulkan_library(),
        ));
        namespace.push(NamespaceEntry::File(FileEntry {
            path: Path::new("/usr/share/vulkan/icd.d").join(self.vulkan_manifest()),
            metadata: Metadata {
                mode: 0o444,
                uid: 0,
                gid: 0,
            },
            source: FileSource::Immutable(Arc::from(contents.into_bytes())),
        }));
        Ok(namespace)
    }

    pub(super) fn accelerator(
        &self,
        drivers: &crate::runtime::drivers::Drivers,
    ) -> Vec<NamespaceEntry> {
        Self::compute()
            .into_iter()
            .map(|(family, name)| self.bind(drivers.path(family, name), name))
            .collect()
    }

    /// Husklet's drivers first, then whatever the launching process already asked for.
    pub(super) fn search_path(directory: &str, inherited: Option<&str>) -> String {
        inherited.filter(|value| !value.is_empty()).map_or_else(
            || directory.to_owned(),
            |value| format!("{directory}:{value}"),
        )
    }
}

#[cfg(test)]
#[path = "projection_test.rs"]
mod tests;
