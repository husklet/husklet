use hl_isa::GuestArchitecture;
use hl_linux::{UTS_FIELD_SIZE, UTS_SIZE, UtsName};

#[test]
fn engine_identity_isas() {
    for (architecture, machine) in [
        (GuestArchitecture::Aarch64, b"aarch64".as_slice()),
        (GuestArchitecture::X86_64, b"x86_64".as_slice()),
    ] {
        let name = UtsName::engine(architecture);
        assert_eq!(name.bytes().len(), UTS_SIZE);
        for (index, expected) in [b"Linux".as_slice(), b"jit", b"6.1.0", b"#1 jit", machine, b""]
            .into_iter()
            .enumerate()
        {
            let start = index * UTS_FIELD_SIZE;
            assert_eq!(&name.bytes()[start..start + expected.len()], expected);
            assert_eq!(name.bytes()[start + expected.len()], 0);
        }
    }
}
