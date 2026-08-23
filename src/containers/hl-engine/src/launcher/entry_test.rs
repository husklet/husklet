//! These four cases moved here from `hl-container`'s `Spec`, which owned the only correct copy of
//! this resolver while `src/apps/engine` carried a second, wrong one. The resolver is now shared,
//! so its tests live beside it.
#![cfg(unix)]

use super::GuestPath;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

fn plant(root: &Path, guest: &str) {
    let path = root.join(guest.trim_start_matches('/'));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"\x7fELF").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hl-guest-path-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn a_layered_only_executable_has_no_false_host_authority() {
    let upper = scratch("authority-upper");
    let lower = scratch("authority-lower");
    assert_eq!(
        GuestPath::host_executable(Path::new("/bin/sh"), &[upper.clone(), lower.clone()]),
        None
    );
    std::fs::remove_dir_all(&upper).unwrap();
    std::fs::remove_dir_all(&lower).unwrap();
}

/// The stock-image case: every real distribution ships `/bin/sh` as an **absolute** link, and an
/// absolute guest target must be re-anchored at the rootfs rather than handed to the host root.
#[test]
fn absolute_image_symlink_resolves_inside_the_rootfs() {
    let root = scratch("absolute-symlink");
    plant(&root, "/bin/busybox");
    symlink("/bin/busybox", root.join("bin/true")).unwrap();
    assert_eq!(
        GuestPath::host_executable(Path::new("/bin/true"), std::slice::from_ref(&root)),
        Some(root.join("bin/busybox"))
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// An absolute link is re-anchored even when its target names a host file that really exists and
/// really is executable, so the answer cannot depend on what is installed outside the image. The
/// decoy is this test binary itself, which is the one host executable every host is known to have.
#[test]
fn an_absolute_symlink_never_reads_the_host_copy_of_its_target() {
    let root = scratch("absolute-host-decoy");
    let decoy = std::env::current_exe().unwrap();
    assert!(GuestPath::executable_here(&decoy));
    symlink(&decoy, root.join("entry")).unwrap();
    assert_eq!(
        GuestPath::host_executable(Path::new("/entry"), std::slice::from_ref(&root)),
        None
    );
    plant(&root, decoy.to_str().unwrap());
    assert_eq!(
        GuestPath::host_executable(Path::new("/entry"), std::slice::from_ref(&root)),
        Some(root.join(decoy.strip_prefix("/").unwrap()))
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn relative_image_symlink_resolves_from_its_guest_parent() {
    let root = scratch("relative-symlink");
    plant(&root, "/usr/lib/tool");
    std::fs::create_dir_all(root.join("usr/bin")).unwrap();
    symlink("../lib/tool", root.join("usr/bin/tool")).unwrap();
    assert_eq!(
        GuestPath::host_executable(Path::new("/usr/bin/tool"), std::slice::from_ref(&root)),
        Some(root.join("usr/lib/tool"))
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// A chain of links resolves, and it is the bound -- not the shape of the chain -- that stops a
/// loop. `LINK_LIMIT` follows one link per pass, so the deepest resolvable chain is one link
/// shorter than the bound.
///
/// `CHAIN` is a literal on purpose. Written as `GuestPath::LINK_LIMIT - 1` this case was
/// **vacuous**: clamping the constant to 39 shortened the fixture by exactly one link too, and the
/// case stayed green through the mutation it exists to catch. A test whose fixture is derived from
/// the constant under test cannot fail when that constant moves. The equality below is what turns a
/// deliberate change to the bound into a test to update rather than into nothing at all.
#[test]
fn a_link_chain_resolves_up_to_the_bound_and_a_loop_does_not() {
    const CHAIN: usize = 39;
    assert_eq!(GuestPath::LINK_LIMIT, CHAIN + 1);

    let root = scratch("chain");
    plant(&root, "/bin/busybox");
    symlink("/bin/busybox", root.join("bin/link-0")).unwrap();
    for step in 1..CHAIN {
        symlink(format!("/bin/link-{}", step - 1), root.join(format!("bin/link-{step}"))).unwrap();
    }
    assert_eq!(
        GuestPath::host_executable(
            Path::new(&format!("/bin/link-{}", CHAIN - 1)),
            std::slice::from_ref(&root)
        ),
        Some(root.join("bin/busybox"))
    );
    symlink("loop-b", root.join("bin/loop-a")).unwrap();
    symlink("loop-a", root.join("bin/loop-b")).unwrap();
    assert_eq!(
        GuestPath::host_executable(Path::new("/bin/loop-a"), std::slice::from_ref(&root)),
        None
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_root_escape_has_no_host_authority() {
    let root = scratch("unsafe-symlink");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    symlink("../../../../bin/sh", root.join("bin/escape")).unwrap();
    assert_eq!(
        GuestPath::host_executable(Path::new("/bin/escape"), std::slice::from_ref(&root)),
        None
    );
    std::fs::remove_dir_all(&root).unwrap();
}
