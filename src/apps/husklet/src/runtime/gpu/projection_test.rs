use super::*;
use std::collections::BTreeSet;

/// A complete on-disk driver set, so a projection can be composed without the packaging step.
struct Fixture;

impl Fixture {
    fn open(root: &Path) -> crate::runtime::drivers::Drivers {
        for (family, name) in [
            ("gl", "libEGL.so.1"),
            ("gl", "libGLESv2.so.2"),
            ("gl", "libwayland-egl.so.1"),
            ("gl", "libgbm.so.1"),
            ("vulkan", "libvk_hl.so.1"),
            ("vulkan", "icd.json"),
            ("cuda", "libcuda.so.1"),
            ("cuda", "libcudart.so.1"),
            ("nvml", "libnvidia-ml.so.1"),
        ] {
            let directory = root.join(family).join("aarch64");
            fs::create_dir_all(&directory).unwrap();
            let contents: &[u8] = if name == "icd.json" {
                br#"{"ICD":{"library_path":"./libvk_hl.so"}}"#
            } else {
                name.as_bytes()
            };
            fs::write(directory.join(name), contents).unwrap();
        }
        crate::runtime::drivers::Drivers::open(root, hl_ws::Arch::Arm64, true, true).unwrap()
    }
}

/// The defect: a read-only bind over `/usr/lib/<multiarch>/libgbm.so.1` makes `rename(2)` from
/// dpkg's `.dpkg-new` staging file fail with EROFS, so no GL package can be unpacked. Nothing
/// Husklet projects may claim a path the distribution's package manager owns.
#[test]
fn projection_leaves_every_distribution_library_path_to_the_package_manager() {
    let root = tempfile::tempdir().unwrap();
    let drivers = Fixture::open(root.path());
    for (arch, multiarch) in [
        (hl_ws::Arch::Arm64, "/usr/lib/aarch64-linux-gnu"),
        (hl_ws::Arch::Amd64, "/usr/lib/x86_64-linux-gnu"),
    ] {
        let projection = Projection::new(arch);
        let directory = Path::new(projection.directory());
        let manifest = Path::new("/usr/share/vulkan/icd.d").join(projection.vulkan_manifest());
        let entries = projection
            .graphics(&drivers)
            .unwrap()
            .into_iter()
            .chain(projection.accelerator(&drivers))
            .map(|entry| entry.path().to_path_buf())
            .filter(|path| path != directory && *path != manifest);
        for path in entries {
            assert_ne!(
                path.parent().unwrap(),
                Path::new(multiarch),
                "{} is owned by the guest package manager",
                path.display()
            );
            assert_eq!(
                path.parent().unwrap(),
                directory,
                "{} escapes the Husklet driver directory",
                path.display()
            );
        }
    }
}

#[test]
fn every_requested_soname_resolves_inside_the_driver_directory() {
    let root = tempfile::tempdir().unwrap();
    let drivers = Fixture::open(root.path());
    let projection = Projection::new(hl_ws::Arch::Arm64);
    let directory = Path::new(projection.directory());
    let namespace = projection.graphics(&drivers).unwrap();
    let bound = namespace
        .iter()
        .filter_map(|entry| match entry {
            NamespaceEntry::HostBind(bind) => Some((bind.path.clone(), bind.host.clone())),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();

    for name in [
        "libEGL.so",
        "libEGL.so.1",
        "libGLESv2.so",
        "libGLESv2.so.2",
        "libgbm.so",
        "libgbm.so.1",
        "libgbm.so.1.0.0",
        "libvk_hl.so",
        "libvk_hl.so.1",
        "libvulkan_freedreno.so",
    ] {
        let path = directory.join(name);
        let host = bound.get(&path).unwrap_or_else(|| panic!("{name}"));
        assert!(host.is_file(), "{name}");
    }
    assert!(bound.values().all(|host| host.starts_with(root.path())));
    // Read-only is the correct access for Husklet's own directory: nothing in the guest owns it.
    assert!(namespace.iter().all(|entry| !matches!(
        entry,
        NamespaceEntry::HostBind(HostBindEntry {
            access: BindAccess::ReadWrite,
            ..
        })
    )));
}

#[test]
fn vulkan_manifest_stays_sandbox_readable_and_names_an_absolute_driver_path() {
    let root = tempfile::tempdir().unwrap();
    let drivers = Fixture::open(root.path());
    for (arch, manifest, library) in [
        (
            hl_ws::Arch::Arm64,
            "/usr/share/vulkan/icd.d/freedreno_icd.aarch64.json",
            "/usr/lib/aarch64-linux-gnu/husklet/libvulkan_freedreno.so",
        ),
        (
            hl_ws::Arch::Amd64,
            "/usr/share/vulkan/icd.d/intel_icd.x86_64.json",
            "/usr/lib/x86_64-linux-gnu/husklet/libvulkan_intel.so",
        ),
    ] {
        let projection = Projection::new(arch);
        let namespace = projection.graphics(&drivers).unwrap();
        let entry = namespace
            .iter()
            .find(|entry| entry.path() == Path::new(manifest))
            .unwrap();
        let NamespaceEntry::File(entry) = entry else {
            panic!("the Vulkan manifest must be a sandbox-readable immutable file");
        };
        assert_eq!(entry.metadata.mode, 0o444);
        let FileSource::Immutable(contents) = &entry.source else {
            unreachable!()
        };
        assert_eq!(
            std::str::from_utf8(contents).unwrap(),
            format!(r#"{{"ICD":{{"library_path":"{library}"}}}}"#)
        );
    }
}

#[test]
fn accelerator_libraries_never_shadow_the_distribution_copies() {
    let root = tempfile::tempdir().unwrap();
    let drivers = Fixture::open(root.path());
    let projection = Projection::new(hl_ws::Arch::Amd64);
    let paths = projection
        .accelerator(&drivers)
        .iter()
        .map(|entry| entry.path().to_path_buf())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        paths,
        BTreeSet::from([
            PathBuf::from("/usr/lib/x86_64-linux-gnu/husklet/libcuda.so.1"),
            PathBuf::from("/usr/lib/x86_64-linux-gnu/husklet/libcudart.so.1"),
            PathBuf::from("/usr/lib/x86_64-linux-gnu/husklet/libnvidia-ml.so.1"),
        ])
    );
}

#[test]
fn the_driver_directory_leads_the_inherited_loader_search_path() {
    let directory = Projection::new(hl_ws::Arch::Arm64).directory();

    assert_eq!(directory, "/usr/lib/aarch64-linux-gnu/husklet");
    assert_eq!(
        Projection::search_path(directory, None),
        "/usr/lib/aarch64-linux-gnu/husklet"
    );
    assert_eq!(
        Projection::search_path(directory, Some("")),
        "/usr/lib/aarch64-linux-gnu/husklet"
    );
    assert_eq!(
        Projection::search_path(directory, Some("/opt/chrome")),
        "/usr/lib/aarch64-linux-gnu/husklet:/opt/chrome"
    );
}

/// The defect: projecting the driver directory itself turned it into a mount of the (empty)
/// materialized tree, so `readdir` reported nothing inside it. The binds stayed openable by exact
/// path, but every listing of the loader search path came back empty, which reads as "the drivers
/// never arrived". Each bind materializes its own mount point, so the directory must stay unclaimed.
#[test]
fn the_driver_directory_itself_is_never_projected() {
    let root = tempfile::tempdir().unwrap();
    let drivers = Fixture::open(root.path());
    let projection = Projection::new(hl_ws::Arch::Arm64);
    let namespace = projection
        .graphics(&drivers)
        .unwrap()
        .into_iter()
        .chain(projection.accelerator(&drivers));

    for entry in namespace {
        assert!(
            !matches!(entry, NamespaceEntry::Directory(_)),
            "{} is projected as a directory",
            entry.path().display()
        );
        assert_ne!(entry.path(), Path::new(projection.directory()));
    }
}

/// The engine's alias projection forces every guest path carrying an aliased basename read-only,
/// so a loader name alias for `libgbm.so.1` breaks dpkg the moment Mesa's own copy appears beside
/// it. Composing the real request would start the GPU and compositor services, so guard the
/// composition source instead. The needles live here, outside the file being inspected.
#[test]
fn the_graphics_extension_declares_no_loader_alias_rules() {
    let source = include_str!("../gpu.rs");

    assert!(source.contains("rules: Vec::new(),"));
    assert!(!source.contains("NameBindEntry"));
    assert!(!source.contains("name-bind-read-only"));
    assert!(!source.contains("\"/usr/lib/aarch64-linux-gnu\""));
}

#[test]
fn guest_environment_leaves_vulkan_discovery_to_the_projected_manifest() {
    let environment = Projection::guest_environment();

    assert!(!environment.contains_key("VK_ICD_FILENAMES"));
    assert!(!environment.contains_key("LD_LIBRARY_PATH"));
    assert_eq!(environment.get("WAYLAND_DISPLAY").unwrap(), "wayland-0");
}
