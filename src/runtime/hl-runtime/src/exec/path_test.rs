use std::sync::Arc;

use hl_isa::GuestArchitecture;
use hl_linux::{ProcessAbi, ProcessMarshalError};

use super::{Fixture, Memory, Node};
use crate::{RuntimeExecError, SourceFactory};

#[test]
fn isa_string_termination() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let invalid_utf8 = b"/bin/\xff";
        fixture.host.add(invalid_utf8, Node::regular(b"elf", true));
        let plan = Fixture::execve_plan(architecture, invalid_utf8);
        assert!(fixture.factory().open(fixture.process, &plan).is_ok());

        fixture.host.add(b"/bin/app", Node::regular(b"elf", true));
        let terminated = Fixture::execve_plan(architecture, b"/bin/app\0ignored");
        assert_eq!(terminated.path, b"/bin/app");
        assert!(fixture.factory().open(fixture.process, &terminated).is_ok());
    }
}

#[test]
fn path_exact_errors() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory {
            bytes: Arc::new(vec![b'x'; 4097]),
        };
        assert_eq!(
            ProcessAbi::new(&memory, architecture).execve(1, 0, 0),
            Err(ProcessMarshalError::NameTooLong),
        );

        let fixture = Fixture::new();
        assert!(matches!(
            fixture
                .factory()
                .open(fixture.process, &Fixture::plan(&vec![b'x'; 4096], None, 0),),
            Err(RuntimeExecError::NameTooLong),
        ));
        assert!(matches!(
            fixture
                .factory()
                .open(fixture.process, &Fixture::plan(b"/bin\0app", None, 0),),
            Err(RuntimeExecError::Invalid),
        ));
    }
}
