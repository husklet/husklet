//! Guest driver set selected by workspace architecture and requested capabilities.

use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub(super) struct Drivers {
    directory: PathBuf,
    arch: &'static str,
}

impl Drivers {
    pub(super) fn open(
        directory: impl Into<PathBuf>,
        arch: hl_ws::Arch,
        gui: bool,
        cuda: bool,
    ) -> io::Result<Self> {
        let drivers = Self {
            directory: directory.into(),
            arch: match arch {
                hl_ws::Arch::Arm64 => "aarch64",
                hl_ws::Arch::Amd64 => "x86_64",
            },
        };
        let mut required = Vec::new();
        if gui {
            required.extend([
                ("gl", "libEGL.so.1"),
                ("gl", "libGLESv2.so.2"),
                ("gl", "libwayland-egl.so.1"),
                ("vulkan", "libvk_hl.so.1"),
                ("vulkan", "icd.json"),
            ]);
        }
        if cuda {
            required.extend([
                ("cuda", "libcuda.so.1"),
                ("cuda", "libcudart.so.1"),
                ("nvml", "libnvidia-ml.so.1"),
            ]);
        }
        for (family, name) in required {
            let path = drivers.path(family, name);
            if !path.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "required {arch:?} guest driver is missing: {}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(drivers)
    }

    pub(super) fn path(&self, family: &str, name: &str) -> PathBuf {
        self.directory.join(family).join(self.arch).join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::Drivers;

    #[test]
    fn reports_the_exact_missing_driver_before_launch() {
        let root = tempfile::tempdir().unwrap();
        let error = Drivers::open(root.path(), hl_ws::Arch::Arm64, true, false).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(error.to_string().contains("gl/aarch64/libEGL.so.1"));
    }

    #[test]
    fn accepts_a_complete_requested_driver_set() {
        let root = tempfile::tempdir().unwrap();
        for (family, name) in [
            ("gl", "libEGL.so.1"),
            ("gl", "libGLESv2.so.2"),
            ("gl", "libwayland-egl.so.1"),
            ("vulkan", "libvk_hl.so.1"),
            ("vulkan", "icd.json"),
        ] {
            let directory = root.path().join(family).join("aarch64");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(name), b"fixture").unwrap();
        }

        let drivers = Drivers::open(root.path(), hl_ws::Arch::Arm64, true, false).unwrap();
        assert_eq!(drivers.directory, root.path());
    }
}
