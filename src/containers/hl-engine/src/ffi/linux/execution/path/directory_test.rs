use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::directory::State;
use super::{NativePath, watch};
use hl_descriptor::{DescriptorFlags, DescriptorTable, ObjectError, SeekPosition, StatusFlags};
use hl_linux::{OpenAbiPlan, PathOperand, ResolveFlags};
use hl_runtime::{
    AccessIdentity, Capabilities, ExecutablePath, GuestPath, GuestPathBytes, OpenDirectory, OpenIntent, RuntimePathHost,
};

static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
fn open_wait_classification() {
    let path = std::env::temp_dir().join(format!(
        "hl-open-wait-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("regular"), b"").unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let plan = |name: &'static [u8], intent: u32, nonblocking| OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(name).unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(intent),
        mode: 0,
        close_on_exec: false,
        nonblocking,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };

    assert!(
        !host
            .open_may_block(&base, &plan(b"/regular", OpenIntent::READ, false))
            .unwrap()
    );
    assert!(
        !host
            .open_may_block(&base, &plan(b"/missing", OpenIntent::READ, false))
            .unwrap_or(false)
    );
    assert!(
        host.open_may_block(&base, &plan(b"/fifo", OpenIntent::READ, false))
            .unwrap()
    );
    assert!(
        host.open_may_block(&base, &plan(b"/fifo", OpenIntent::WRITE, false))
            .unwrap()
    );
    assert!(
        !host
            .open_may_block(&base, &plan(b"/fifo", OpenIntent::READ, true))
            .unwrap()
    );
    assert!(
        !host
            .open_may_block(&base, &plan(b"/fifo", OpenIntent::READ | OpenIntent::WRITE, false),)
            .unwrap()
    );
    assert!(
        !host
            .open_may_block(&base, &plan(b"/proc/self/stat", OpenIntent::READ, false))
            .unwrap_or(false)
    );

    let swap = plan(b"/regular", OpenIntent::READ, false);
    let mut prepared = host.prepare_open(&base, &swap).unwrap();
    std::fs::remove_file(path.join("regular")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path.join("regular"))
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(prepared.commit(), Err(hl_runtime::RuntimePathError::WouldBlock));
    assert!(host.open_may_block(&base, &swap).unwrap());

    let reverse = plan(b"/fifo", OpenIntent::READ, false);
    assert!(host.open_may_block(&base, &reverse).unwrap());
    std::fs::remove_file(path.join("fifo")).unwrap();
    std::fs::write(path.join("fifo"), b"").unwrap();
    let mut prepared = host.prepare_open(&base, &reverse).unwrap();
    prepared.commit().unwrap();

    let projected = NativePath::projected(b"/workspace", watch::Hub::projected(b"/workspace").unwrap()).unwrap();
    assert!(
        projected
            .open_may_block(
                &projected.root_base().unwrap(),
                &plan(b"/provider", OpenIntent::READ, false),
            )
            .unwrap()
    );
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn builtin_descriptor_preserves_filesystem() {
    let path = std::env::temp_dir().join(format!(
        "hl-builtin-filesystem-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: hl_runtime::OpenDirectory::default(),
            path: GuestPathBytes::new(b"/dev/null").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(OpenIntent::READ),
        mode: 0,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    let mut opened = host.prepare_open(&host.root_base().unwrap(), &plan).unwrap();
    let object = opened.object();
    opened.commit().unwrap();
    let table = DescriptorTable::new(4).unwrap();
    let install = table
        .prepare_open(0, object, StatusFlags::default(), DescriptorFlags::default())
        .unwrap();
    let descriptor = install.publish();
    let node = host.descriptor_node(table.pin(descriptor).unwrap()).unwrap();
    let filesystem = node.filesystem().unwrap();
    assert_eq!(filesystem.kind, hl_runtime::FilesystemKind::Tmpfs);
    assert!(filesystem.block_size > 0);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn commit_is_transactional() {
    let path = std::env::temp_dir().join(format!(
        "hl-directory-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("b"), b"").unwrap();
    std::fs::write(path.join("a"), b"").unwrap();

    let mut directory = State::new(path.clone());
    let first = directory.read(1).unwrap();
    let retry = directory.read(1).unwrap();
    assert_eq!(first, retry);
    assert_eq!(first.entries[0].name, b".");
    directory.commit(first.token, 1).unwrap();
    let second = directory.read(1).unwrap();
    assert_eq!(second.entries[0].name, b"..");
    assert_eq!(directory.seek(0).unwrap(), 0);
    assert_eq!(directory.read(1).unwrap().entries[0].name, b".");
    assert_eq!(directory.seek(usize::MAX as u64), Err(ObjectError::InvalidArgument));

    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn root_confined() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
        "hl-path-root-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("inside"), b"image").unwrap();
    std::fs::set_permissions(path.join("inside"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let inside = host
        .resolve_executable(
            &base,
            &ExecutablePath {
                path: GuestPathBytes::new(b"//inside").unwrap(),
                nofollow: false,
            },
        )
        .unwrap();
    assert_eq!(inside.read_image(16).unwrap(), b"image");
    assert!(
        host.resolve_executable(
            &base,
            &ExecutablePath {
                path: GuestPathBytes::new(b"/../../outside").unwrap(),
                nofollow: false,
            },
        )
        .is_err()
    );

    drop(inside);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn projected_root_is_a_directory_without_remote_lookup() {
    let host = NativePath::projected(b"/workspace", watch::Hub::projected(b"/workspace").unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let operand = PathOperand {
        directory: OpenDirectory::default(),
        path: GuestPathBytes::new(b"/").unwrap(),
        allow_empty: false,
        nofollow: false,
    };
    assert_eq!(
        host.directory_path(&base, &operand).unwrap(),
        GuestPath::new("/").unwrap()
    );
}

#[test]
fn pinned_swap_safe() {
    let path = std::env::temp_dir().join(format!(
        "hl-path-pin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(path.join("inside")).unwrap();
    std::fs::create_dir(path.join("outside")).unwrap();
    std::fs::write(path.join("inside/file"), b"inside").unwrap();
    std::fs::write(path.join("outside/file"), b"outside").unwrap();
    std::os::unix::fs::symlink("inside", path.join("link")).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/link/file").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(OpenIntent::READ),
        mode: 0,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    let mut prepared = host.prepare_open(&base, &plan).unwrap();
    let object = prepared.object();
    std::fs::remove_file(path.join("link")).unwrap();
    std::os::unix::fs::symlink("outside", path.join("link")).unwrap();
    prepared.commit().unwrap();
    let mut output = [0_u8; 7];
    assert_eq!(object.read(&mut output).unwrap(), 6);
    assert_eq!(&output[..6], b"inside");

    drop(object);
    drop(prepared);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn image_swap_safe() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
        "hl-image-pin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("image"), b"original").unwrap();
    std::fs::set_permissions(path.join("image"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let image = host
        .resolve_executable(
            &base,
            &ExecutablePath {
                path: GuestPathBytes::new(b"/image").unwrap(),
                nofollow: false,
            },
        )
        .unwrap();
    std::fs::rename(path.join("image"), path.join("old")).unwrap();
    std::fs::write(path.join("image"), b"replacement").unwrap();
    assert_eq!(image.read_image(64).unwrap(), b"original");

    drop(image);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn opath_contract() {
    let path = std::env::temp_dir().join(format!(
        "hl-opath-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("file"), b"data").unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap())
        .unwrap()
        .with_read_only(true);
    let base = host.root_base().unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/file").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(
            OpenIntent::READ
                | OpenIntent::WRITE
                | OpenIntent::CREATE
                | OpenIntent::TRUNCATE
                | OpenIntent::TEMPORARY
                | OpenIntent::PATH_ONLY,
        ),
        mode: 0o600,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    let mut prepared = host.prepare_open(&base, &plan).unwrap();
    let object = prepared.object();
    prepared.commit().unwrap();
    assert_eq!(object.metadata().unwrap().size, 4);
    assert_eq!(object.read(&mut [0_u8; 1]), Err(ObjectError::BadDescriptor));
    assert_eq!(object.write(b"x"), Err(ObjectError::BadDescriptor));
    assert_eq!(object.read_at(0, &mut [0_u8; 1]), Err(ObjectError::BadDescriptor));
    assert_eq!(object.write_at(0, b"x"), Err(ObjectError::BadDescriptor));
    assert_eq!(object.seek(SeekPosition::Start(0)), Err(ObjectError::BadDescriptor));
    assert_eq!(std::fs::read(path.join("file")).unwrap(), b"data");
    drop(object);
    drop(prepared);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn unlinked_metadata() {
    let path = std::env::temp_dir().join(format!(
        "hl-unlinked-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("file"), b"data").unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/file").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(OpenIntent::READ),
        mode: 0,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    let mut opened = host.prepare_open(&base, &plan).unwrap();
    let object = opened.object();
    opened.commit().unwrap();
    std::fs::remove_file(path.join("file")).unwrap();
    assert_eq!(object.metadata().unwrap().links, 0);

    drop(object);
    drop(opened);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn readonly_open() {
    let path = std::env::temp_dir().join(format!(
        "hl-readonly-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("file"), b"data").unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap())
        .unwrap()
        .with_read_only(true);
    let base = host.root_base().unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/file").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(OpenIntent::WRITE | OpenIntent::TRUNCATE),
        mode: 0,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    assert_eq!(
        host.prepare_open(&base, &plan).unwrap_err(),
        hl_runtime::RuntimePathError::ReadOnly,
    );
    assert_eq!(std::fs::read(path.join("file")).unwrap(), b"data");
    let mutation = hl_linux::FsMutationPlan::CreateDirectory {
        target: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/created").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        mode: 0o755,
    };
    let identity = AccessIdentity {
        user: 0,
        group: 0,
        supplementary_groups: Vec::new(),
        capabilities: Capabilities {
            dac_override: true,
            ..Capabilities::default()
        },
    };
    assert_eq!(
        host.prepare_mutation(&[base], &mutation, &identity).unwrap_err(),
        hl_runtime::RuntimePathError::ReadOnly,
    );
    assert!(!path.join("created").exists());

    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn hardlink_replacement() {
    let path = std::env::temp_dir().join(format!(
        "hl-link-pin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("source"), b"original").unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let source_base = host.root_base().unwrap();
    let target_base = host.root_base().unwrap();
    let operand = |name: &'static [u8]| PathOperand {
        directory: OpenDirectory::default(),
        path: GuestPathBytes::new(name).unwrap(),
        allow_empty: false,
        nofollow: false,
    };
    let plan = hl_linux::FsMutationPlan::Link {
        from: operand(b"/source"),
        to: operand(b"/published"),
        follow: true,
    };
    let identity = AccessIdentity {
        user: 0,
        group: 0,
        supplementary_groups: Vec::new(),
        capabilities: Capabilities {
            dac_override: true,
            dac_read_search: true,
            ..Capabilities::default()
        },
    };
    let mut prepared = host
        .prepare_mutation(&[source_base, target_base], &plan, &identity)
        .unwrap();
    std::fs::rename(path.join("source"), path.join("original")).unwrap();
    std::fs::write(path.join("source"), b"replacement").unwrap();
    prepared.commit().unwrap();
    assert_eq!(std::fs::read(path.join("published")).unwrap(), b"original");
    drop(prepared);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn mutation_trailing_slash_requires_directory() {
    let path = std::env::temp_dir().join(format!(
        "hl-trailing-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("source"), b"data").unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let plan = hl_linux::FsMutationPlan::Rename {
        from: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/source/").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        to: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/target").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        exchange: false,
        no_replace: false,
    };
    let identity = AccessIdentity {
        user: 0,
        group: 0,
        supplementary_groups: Vec::new(),
        capabilities: Capabilities {
            dac_override: true,
            ..Capabilities::default()
        },
    };
    assert_eq!(
        host.prepare_mutation(
            &[host.root_base().unwrap(), host.root_base().unwrap()],
            &plan,
            &identity
        )
        .unwrap_err(),
        hl_runtime::RuntimePathError::NotDirectory,
    );
    assert!(path.join("source").exists());
    assert!(!path.join("target").exists());
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn chmod_replacement() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
        "hl-chmod-pin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("target"), b"original").unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let plan = hl_linux::FsMutationPlan::Chmod {
        target: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/target").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        mode: 0o600,
    };
    let identity = AccessIdentity {
        user: std::fs::metadata(path.join("target")).unwrap().uid(),
        group: 0,
        supplementary_groups: Vec::new(),
        capabilities: Capabilities::default(),
    };
    let mut prepared = host.prepare_mutation(&[base], &plan, &identity).unwrap();
    std::fs::rename(path.join("target"), path.join("original")).unwrap();
    std::fs::write(path.join("target"), b"replacement").unwrap();
    std::fs::set_permissions(path.join("target"), std::fs::Permissions::from_mode(0o644)).unwrap();
    prepared.commit().unwrap();
    assert_eq!(std::fs::metadata(path.join("original")).unwrap().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(path.join("target")).unwrap().mode() & 0o777, 0o644);
    drop(prepared);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn tmpfile_anonymous() {
    let path = std::env::temp_dir().join(format!(
        "hl-otmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(path.join("scratch")).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap()).unwrap();
    let base = host.root_base().unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/scratch").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(
            OpenIntent::READ | OpenIntent::WRITE | OpenIntent::DIRECTORY | OpenIntent::TEMPORARY,
        ),
        mode: 0o600,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    let mut prepared = host.prepare_open(&base, &plan).unwrap();
    let object = prepared.object();
    prepared.commit().unwrap();
    assert_eq!(object.write(b"anonymous").unwrap(), 9);
    let mut output = [0_u8; 9];
    assert_eq!(object.read_at(0, &mut output).unwrap(), 9);
    assert_eq!(&output, b"anonymous");
    assert_eq!(object.metadata().unwrap().links, 0);
    let metadata = object.metadata().unwrap();
    assert!(
        !host
            .paths
            .lock()
            .unwrap()
            .contains_key(&(metadata.device, metadata.inode))
    );
    assert_eq!(std::fs::read_dir(path.join("scratch")).unwrap().count(), 0);

    drop(object);
    drop(prepared);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn tmpfile_descriptor_link() {
    let path = std::env::temp_dir().join(format!(
        "hl-otmp-link-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(path.join("scratch")).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let registry = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
    let mut credentials = hl_task::ProcessCredentials::new(0, 0, &[], 32).unwrap();
    credentials.capabilities.effective |= hl_task::CapabilitySets::DAC_READ_SEARCH;
    let (process, _) = registry
        .create_init(credentials, hl_task::ProcessLimits::empty())
        .unwrap();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap())
        .unwrap()
        .with_process(
            Arc::clone(&registry),
            process,
            Arc::new(hl_runtime::NamespaceHandleRegistry::new()),
            Arc::new(hl_descriptor::DescriptorTable::new(8).unwrap()),
        );
    let base = host.root_base().unwrap();
    let plan = OpenAbiPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/scratch").unwrap(),
            allow_empty: false,
            nofollow: false,
        },
        intent: OpenIntent::from_bits(OpenIntent::WRITE | OpenIntent::DIRECTORY | OpenIntent::TEMPORARY),
        mode: 0o600,
        close_on_exec: false,
        nonblocking: false,
        no_controlling_terminal: false,
        resolve: ResolveFlags::default(),
    };
    let mut opened = host.prepare_open(&base, &plan).unwrap();
    let object = opened.object();
    opened.commit().unwrap();
    assert_eq!(object.write(b"linked").unwrap(), 6);
    let table = DescriptorTable::new(8).unwrap();
    let descriptor = table.install(0, object, DescriptorFlags::default()).unwrap();
    let target = PathOperand {
        directory: OpenDirectory::default(),
        path: GuestPathBytes::new(b"/scratch/materialized").unwrap(),
        allow_empty: false,
        nofollow: false,
    };
    let identity = AccessIdentity {
        user: 0,
        group: 0,
        supplementary_groups: Vec::new(),
        capabilities: Capabilities {
            dac_read_search: true,
            ..Capabilities::default()
        },
    };
    let mut linked = host
        .prepare_inode_link(table.pin(descriptor).unwrap(), &base, &target, &identity)
        .unwrap();
    linked.commit().unwrap();
    assert_eq!(std::fs::read(path.join("scratch/materialized")).unwrap(), b"linked");
    assert_eq!(std::fs::metadata(path.join("scratch/materialized")).unwrap().nlink(), 1);

    let exclusive_plan = OpenAbiPlan {
        intent: OpenIntent::from_bits(plan.intent.bits() | OpenIntent::EXCLUSIVE),
        ..plan.clone()
    };
    let mut exclusive = host.prepare_open(&base, &exclusive_plan).unwrap();
    let exclusive_object = exclusive.object();
    exclusive.commit().unwrap();
    let exclusive_descriptor = table.install(0, exclusive_object, DescriptorFlags::default()).unwrap();
    let exclusive_target = PathOperand {
        path: GuestPathBytes::new(b"/scratch/exclusive").unwrap(),
        ..target.clone()
    };
    let mut rejected = host
        .prepare_inode_link(
            table.pin(exclusive_descriptor).unwrap(),
            &base,
            &exclusive_target,
            &identity,
        )
        .unwrap();
    assert_eq!(rejected.commit(), Err(hl_runtime::RuntimePathError::NotFound));
    assert!(!path.join("scratch/exclusive").exists());

    drop(rejected);
    drop(exclusive);
    drop(linked);
    drop(table);
    drop(opened);
    drop(host);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn credential_projection() {
    let registry = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
    let mut credentials = hl_task::ProcessCredentials::new(0, 0, &[7], 32).unwrap();
    credentials.capabilities.effective |= hl_task::CapabilitySets::DAC_READ_SEARCH;
    let (process, thread) = registry
        .create_init(credentials, hl_task::ProcessLimits::empty())
        .unwrap();
    let path = std::env::temp_dir().join(format!(
        "hl-identity-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    let bytes = path.as_os_str().as_encoded_bytes();
    let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap())
        .unwrap()
        .with_process(
            Arc::clone(&registry),
            process,
            Arc::new(hl_runtime::NamespaceHandleRegistry::new()),
            Arc::new(hl_descriptor::DescriptorTable::new(8).unwrap()),
        );
    let parent = host.access_identity().unwrap();
    assert_eq!((parent.user, parent.group), (0, 0));
    assert_eq!(parent.supplementary_groups, [7]);
    assert!(parent.capabilities.dac_read_search);

    let (child, child_thread) = registry
        .commit_fork_process(registry.begin_fork_process(thread).unwrap())
        .unwrap();
    let child_host = host.for_test(
        child,
        child_thread,
        Arc::new(hl_descriptor::DescriptorTable::new(8).unwrap()),
        Arc::new(hl_runtime::WorkingDirectory::root()),
        Arc::new(hl_runtime::FsContext::default()),
    );
    let mut dropped = registry.credentials(process).unwrap();
    dropped.filesystem_user = 1000;
    dropped.filesystem_group = 1001;
    dropped.capabilities.effective = 0;
    registry.replace_credentials(process, dropped).unwrap();
    let parent = host.access_identity().unwrap();
    assert_eq!((parent.user, parent.group), (1000, 1001));
    assert!(!parent.capabilities.dac_read_search);
    assert!(child_host.access_identity().unwrap().capabilities.dac_read_search);

    drop(child_host);
    drop(host);
    drop(registry);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn devpts_snapshot_lifetime() {
    let path = std::env::temp_dir().join(format!(
        "hl-devpts-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("ptmx"), b"").unwrap();
    std::fs::write(path.join("99"), b"").unwrap();
    let terminals = std::sync::Arc::new(hl_runtime::TerminalCatalog::default());
    let first = terminals.allocate().unwrap();
    let second = terminals.allocate().unwrap();
    let mut directory = State::new(path.clone()).with_terminals(std::sync::Arc::clone(&terminals));
    let snapshot = directory.read(16).unwrap();
    let names: Vec<_> = snapshot.entries.iter().map(|entry| entry.name.as_slice()).collect();
    assert_eq!(
        names,
        [
            b".".as_slice(),
            b"..".as_slice(),
            b"0".as_slice(),
            b"1".as_slice(),
            b"ptmx".as_slice(),
        ],
    );
    assert!(snapshot.entries[2..].iter().all(|entry| entry.file_type == 2));
    assert_eq!(snapshot.entries[2].inode, u64::from(136_u32) << 32);
    assert_eq!(snapshot.entries[4].inode, (u64::from(5_u32) << 32) | 2);

    terminals.retire(first.id()).unwrap();
    let replacement = terminals.allocate().unwrap();
    assert_ne!(replacement.id().generation, first.id().generation);
    assert_eq!(directory.read(16).unwrap(), snapshot);

    terminals.retire(second.id()).unwrap();
    terminals.retire(replacement.id()).unwrap();
    let mut directory = State::new(path.clone()).with_terminals(terminals);
    let fresh = directory.read(16).unwrap();
    let names: Vec<_> = fresh.entries.iter().map(|entry| entry.name.as_slice()).collect();
    assert_eq!(names, [b".".as_slice(), b"..".as_slice(), b"ptmx".as_slice()],);
    std::fs::remove_dir_all(path).unwrap();
}
