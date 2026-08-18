use super::build::select_messages;
use super::{
    Artifact, ElfKind, publish_prefix, require_matching_architecture, require_readelf_contract, temporary_prefix,
};
use std::{fs, io::Cursor};

fn elf(kind: u16, machine: u16) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
    bytes[16..18].copy_from_slice(&kind.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes
}

#[test]
fn cargo_messages_bind_one_runner_to_one_native_library() {
    let messages = concat!(
        "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///source/src/apps/testing#0.1.0\",\"target\":{\"name\":\"testing\",\"kind\":[\"bin\"]},\"executable\":\"/build/testing\"}\n",
        "{\"reason\":\"build-script-executed\",\"package_id\":\"path+file:///source/src/runtime/hl-native#0.1.0\",\"env\":[[\"HL_NATIVE_LIBRARY_PATH\",\"/build/libhl_native_engine.so\"]]}\n"
    );
    let testing = "path+file:///source/src/apps/testing#0.1.0";
    let native = "path+file:///source/src/runtime/hl-native#0.1.0";
    let selected = select_messages(Cursor::new(messages), testing, native)
        .unwrap()
        .unwrap();
    assert_eq!(selected.runner, std::path::Path::new("/build/testing"));
    assert_eq!(selected.library, std::path::Path::new("/build/libhl_native_engine.so"));
    assert!(select_messages(Cursor::new(&messages[..messages.find('\n').unwrap()]), testing, native).is_err());
    assert!(select_messages(Cursor::new(format!("{messages}{messages}")), testing, native).is_err());
    assert!(select_messages(Cursor::new(messages), testing, "path+file:///foreign/hl-native#0.1.0").is_err());
    assert!(select_messages(Cursor::new(messages), "path+file:///foreign/testing#0.1.0", native).is_err());
}

#[test]
fn missing_corrupt_symlinked_and_wrong_kind_libraries_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.so");
    assert!(Artifact::settled(&missing, ElfKind::SharedLibrary).is_err());
    let corrupt = directory.path().join("corrupt.so");
    fs::write(&corrupt, b"not an ELF image").unwrap();
    assert!(Artifact::settled(&corrupt, ElfKind::SharedLibrary).is_err());
    let executable = directory.path().join("executable.so");
    fs::write(&executable, elf(2, 183)).unwrap();
    assert!(Artifact::settled(&executable, ElfKind::SharedLibrary).is_err());
    #[cfg(unix)]
    {
        let symlink = directory.path().join("link.so");
        std::os::unix::fs::symlink(&executable, &symlink).unwrap();
        assert!(Artifact::settled(&symlink, ElfKind::SharedLibrary).is_err());
    }
}

#[test]
fn staging_prefix_is_reserved_and_publication_never_replaces_a_name() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("published");
    let temporary = temporary_prefix(&output).unwrap();
    assert!(temporary.is_dir());
    fs::create_dir(&output).unwrap();
    assert!(publish_prefix(&temporary, &output).is_err());
    assert!(temporary.is_dir());
    fs::remove_dir(&output).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing", &output).unwrap();
        assert!(publish_prefix(&temporary, &output).is_err());
        assert!(temporary.is_dir());
    }
}

#[test]
fn a_valid_shared_image_for_the_wrong_architecture_is_rejected() {
    let runner = elf(3, 183);
    assert!(require_matching_architecture(&runner, &elf(3, 62)).is_err());
    assert!(require_matching_architecture(&runner, &elf(3, 183)).is_ok());
    assert!(require_matching_architecture(&elf(3, 8), &elf(3, 8)).is_err());
}

#[test]
fn a_same_architecture_shared_object_with_the_wrong_contract_is_rejected() {
    let runner = " 0x1 (NEEDED) Shared library: [libc.so.6]\n";
    let library = " 0xe (SONAME) Library soname: [libhl_native_engine.so]\n";
    let exports = concat!(
        " 1: 1 1 FUNC GLOBAL DEFAULT 12 hl_engine_abi\n",
        " 2: 1 1 FUNC GLOBAL DEFAULT 12 hl_engine_version\n",
        " 3: 1 1 FUNC GLOBAL DEFAULT 12 hl_c_backend_create\n",
    );
    let expected = "hl_c_backend_create\nhl_engine_abi\nhl_engine_version\n";
    assert!(require_readelf_contract(runner, library, exports, expected).is_ok());
    assert!(require_readelf_contract(runner, library, "", expected).is_err());
    assert!(require_readelf_contract(runner, library, exports, "hl_engine_abi\n").is_err());
    assert!(require_readelf_contract(runner, "SONAME [libm.so.6]", exports, expected).is_err());
    assert!(
        require_readelf_contract(
            "(NEEDED) Shared library: [libhl_native_engine.so]",
            library,
            exports,
            expected,
        )
        .is_err()
    );
}
