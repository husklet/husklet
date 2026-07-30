//! Guest driver set selected by workspace architecture and requested capabilities.

use sha2::Digest as _;
use std::io;
use std::io::Read as _;
use std::path::PathBuf;

const GUI: &[(&str, &str)] = &[
    ("gl", "libEGL.so.1"),
    ("gl", "libGLESv2.so.2"),
    ("gl", "libwayland-egl.so.1"),
    ("gl", "libgbm.so.1"),
    ("vulkan", "libvk_hl.so.1"),
    ("vulkan", "icd.json"),
];
const CUDA: &[(&str, &str)] = &[
    ("cuda", "libcuda.so.1"),
    ("cuda", "libcudart.so.1"),
    ("nvml", "libnvidia-ml.so.1"),
];

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
        for &(family, name) in Self::required(gui, cuda) {
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

    pub(super) fn fingerprint(
        directory: impl Into<PathBuf>,
        arch: hl_ws::Arch,
        gui: bool,
        cuda: bool,
    ) -> io::Result<String> {
        let drivers = Self::open(directory, arch, gui, cuda)?;
        let mut digest = sha2::Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        for &(family, name) in Self::required(gui, cuda) {
            digest.update(family.len().to_le_bytes());
            digest.update(family.as_bytes());
            digest.update(name.len().to_le_bytes());
            digest.update(name.as_bytes());
            let path = drivers.path(family, name);
            let mut file = std::fs::File::open(&path)?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
        }
        Ok(digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    pub(super) fn path(&self, family: &str, name: &str) -> PathBuf {
        self.directory.join(family).join(self.arch).join(name)
    }

    fn required(
        gui: bool,
        cuda: bool,
    ) -> impl Iterator<Item = &'static (&'static str, &'static str)> {
        GUI.iter()
            .filter(move |_| gui)
            .chain(CUDA.iter().filter(move |_| cuda))
    }
}

#[cfg(test)]
mod tests {
    use super::Drivers;
    use std::collections::BTreeMap;

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
            ("gl", "libgbm.so.1"),
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

    #[test]
    fn fingerprint_is_stable_and_covers_exact_requested_artifacts() {
        let root = tempfile::tempdir().unwrap();
        for &(family, name) in GUI {
            let directory = root.path().join(family).join("aarch64");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(name), format!("{family}/{name}")).unwrap();
        }
        let first = Drivers::fingerprint(root.path(), hl_ws::Arch::Arm64, true, false).unwrap();
        let second = Drivers::fingerprint(root.path(), hl_ws::Arch::Arm64, true, false).unwrap();
        assert_eq!(first, second);

        std::fs::write(
            root.path().join("gl/aarch64/libEGL.so.1"),
            b"new generation",
        )
        .unwrap();
        let changed = Drivers::fingerprint(root.path(), hl_ws::Arch::Arm64, true, false).unwrap();
        assert_ne!(first, changed);

        std::fs::create_dir_all(root.path().join("gl/x86_64")).unwrap();
        std::fs::write(root.path().join("gl/x86_64/libEGL.so.1"), b"other arch").unwrap();
        std::fs::write(root.path().join("unrequested"), b"ignored").unwrap();
        assert_eq!(
            changed,
            Drivers::fingerprint(root.path(), hl_ws::Arch::Arm64, true, false).unwrap()
        );
    }

    #[test]
    fn package_manifest_covers_supported_architectures_and_closes_every_link() {
        let manifest = include_str!("../../package/drivers.manifest");
        let mut entries = BTreeMap::new();
        for line in manifest.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            assert!(matches!(fields.as_slice(), ["file", _] | ["link", _, _]));
            let path = fields[1];
            assert!(
                path.contains("/aarch64/") || path.contains("/x86_64/"),
                "{path}"
            );
            assert!(!path.contains(".."), "{path}");
            assert!(entries.insert(path, fields).is_none(), "{path}");
        }
        for fields in entries.values().filter(|fields| fields[0] == "link") {
            let parent = std::path::Path::new(fields[1]).parent().unwrap();
            let target = parent.join(fields[2]);
            assert!(
                entries.contains_key(target.to_str().unwrap()),
                "{}",
                target.display()
            );
        }
        for arch in ["aarch64", "x86_64"] {
            for &(family, name) in super::GUI.iter().chain(super::CUDA) {
                assert!(
                    entries.contains_key(format!("{family}/{arch}/{name}").as_str()),
                    "{arch}/{family}/{name}"
                );
            }
        }
    }
}
