use crate::{
    ExecLoadContext, ExecutionImageBuilder, LoadFailureReporter, LoaderExecImage, LoaderExecParticipant,
    PreparedExecParticipant, RuntimeExecError, RuntimeExecParticipant, SourceFactory, SpaceFactory,
};
use hl_isa::GuestArchitecture;
use hl_loader::{
    AddressSpaceError, GuestCredentials, GuestFeatures, ImageProtectionRegistry, ImageRole, ImageSource,
    ImageSourceError, InitialTlsPlan, LoadLimits, LoadRequest, Loader, MappingKind, MappingPlacement, Protection,
    ReservedMapping, ThreadLocalStorage, TransactionalAddressSpace,
};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
const LINK_BASE: u64 = 0x40_0000;
const HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const DATA_OFFSET: usize = 0x1000;
const INTERPRETER_OFFSET: usize = 0x200;
fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn program_header(
    bytes: &mut [u8],
    index: usize,
    kind: u32,
    flags: u32,
    offset: u64,
    address: u64,
    file_size: u64,
    memory_size: u64,
) {
    let base = HEADER_SIZE + index * PROGRAM_HEADER_SIZE;
    put_u32(bytes, base, kind);
    put_u32(bytes, base + 4, flags);
    put_u64(bytes, base + 8, offset);
    put_u64(bytes, base + 16, address);
    put_u64(bytes, base + 24, address);
    put_u64(bytes, base + 32, file_size);
    put_u64(bytes, base + 40, memory_size);
    put_u64(bytes, base + 48, 0x1000);
}

fn elf(architecture: GuestArchitecture, dynamic: bool, pie: bool) -> Vec<u8> {
    let count = if dynamic { 3 } else { 2 };
    let mut bytes = vec![0; DATA_OFFSET + 0x100];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4..8].copy_from_slice(&[2, 1, 1, 0]);
    put_u16(&mut bytes, 16, if pie { 3 } else { 2 });
    put_u16(&mut bytes, 18, architecture.elf_machine());
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + 0x180);
    put_u64(&mut bytes, 32, HEADER_SIZE as u64);
    put_u16(&mut bytes, 52, HEADER_SIZE as u16);
    put_u16(&mut bytes, 54, PROGRAM_HEADER_SIZE as u16);
    put_u16(&mut bytes, 56, count);
    program_header(&mut bytes, 0, 1, 5, 0, LINK_BASE, 0x300, 0x300);
    program_header(
        &mut bytes,
        1,
        1,
        6,
        DATA_OFFSET as u64,
        LINK_BASE + 0x2000,
        0x100,
        0x280,
    );
    if dynamic {
        let path = b"/lib/ld.so\0";
        bytes[INTERPRETER_OFFSET..INTERPRETER_OFFSET + path.len()].copy_from_slice(path);
        program_header(
            &mut bytes,
            2,
            3,
            4,
            INTERPRETER_OFFSET as u64,
            0,
            path.len() as u64,
            path.len() as u64,
        );
    }
    bytes
}

struct Source {
    main: Vec<u8>,
    interpreter: Vec<u8>,
}

impl ImageSource for Source {
    fn read_image(&mut self, role: ImageRole, _: &[u8], maximum: usize) -> Result<Vec<u8>, ImageSourceError> {
        let bytes = match role {
            ImageRole::Main => &self.main,
            ImageRole::Interpreter => &self.interpreter,
        };
        if bytes.len() > maximum {
            return Err(ImageSourceError::TooLarge);
        }
        Ok(bytes.clone())
    }
}

struct Sources {
    architecture: GuestArchitecture,
    dynamic: bool,
    malformed: bool,
    malformed_interpreter: bool,
    nested_interpreter: bool,
}

impl SourceFactory for Sources {
    type Source = Source;

    fn open(&self, _: hl_task::ProcessId, _: &hl_linux::ExecPlan) -> Result<Self::Source, RuntimeExecError> {
        let mut main = elf(self.architecture, self.dynamic, false);
        if self.malformed {
            main[0] = 0;
        }
        let mut interpreter = elf(self.architecture, self.nested_interpreter, true);
        if self.malformed_interpreter {
            interpreter[0] = 0;
        }
        Ok(Source { main, interpreter })
    }
}

struct AddressSpace {
    next: u32,
    address: u64,
    reservations: BTreeMap<u32, u64>,
    operation: usize,
    fail_at: Option<usize>,
}

impl AddressSpace {
    fn new(fail_at: Option<usize>) -> Self {
        Self {
            next: 1,
            address: 0x90_0000,
            reservations: BTreeMap::new(),
            operation: 0,
            fail_at,
        }
    }

    fn operation(&mut self) -> Result<(), AddressSpaceError> {
        self.operation += 1;
        if self.fail_at == Some(self.operation) {
            Err(AddressSpaceError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn range(&self, token: u32, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        let length = self.reservations.get(&token).ok_or(AddressSpaceError::InvalidRange)?;
        if offset.checked_add(size).is_none_or(|end| end > *length) {
            return Err(AddressSpaceError::InvalidRange);
        }
        Ok(())
    }
}

impl TransactionalAddressSpace for AddressSpace {
    type Reservation = u32;

    fn reserve(
        &mut self,
        _: MappingKind,
        size: u64,
        placement: MappingPlacement,
    ) -> Result<ReservedMapping<u32>, AddressSpaceError> {
        self.operation()?;
        let address = match placement {
            MappingPlacement::Fixed(value) | MappingPlacement::Hint(Some(value)) => value,
            MappingPlacement::Hint(None) => {
                let value = self.address;
                self.address += 0x20_0000;
                value
            }
        };
        let token = self.next;
        self.next += 1;
        self.reservations.insert(token, size);
        Ok(ReservedMapping::new(token, address, size))
    }

    fn stage_write(&mut self, token: &u32, offset: u64, bytes: &[u8]) -> Result<(), AddressSpaceError> {
        self.operation()?;
        self.range(*token, offset, bytes.len() as u64)
    }

    fn stage_zero(&mut self, token: &u32, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        self.operation()?;
        self.range(*token, offset, size)
    }

    fn stage_protection(
        &mut self,
        token: &u32,
        offset: u64,
        size: u64,
        _: Protection,
    ) -> Result<(), AddressSpaceError> {
        self.operation()?;
        self.range(*token, offset, size)
    }

    fn commit(&mut self, _: &[u32]) -> Result<(), AddressSpaceError> {
        self.operation()
    }

    fn rollback(&mut self, token: &u32) {
        self.reservations.remove(token);
    }
}

impl ImageProtectionRegistry<u32> for AddressSpace {
    fn stage_executable(&mut self, token: &u32, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        self.operation()?;
        self.range(*token, offset, size)
    }

    fn stage_guest_access(&mut self, _: &u32, _: u64, _: u64, _: bool) -> Result<(), AddressSpaceError> {
        self.operation()
    }
}

struct Spaces(Option<usize>);

impl SpaceFactory for Spaces {
    type AddressSpace = AddressSpace;

    fn create(&self, _: hl_task::ProcessId) -> Result<Self::AddressSpace, RuntimeExecError> {
        Ok(AddressSpace::new(self.0))
    }
}

struct Context;

impl ExecLoadContext for Context {
    fn random(&self) -> Result<[u8; 16], RuntimeExecError> {
        Ok([7; 16])
    }

    fn credentials(&self, _: hl_task::ProcessId) -> Result<GuestCredentials, RuntimeExecError> {
        Ok(GuestCredentials::default())
    }

    fn features(&self) -> GuestFeatures {
        GuestFeatures::default()
    }
}

struct Tls;

impl ThreadLocalStorage for Tls {
    type Prepared = u64;
    type Error = ();

    fn prepare_initial(&mut self, plan: &InitialTlsPlan) -> Result<Self::Prepared, Self::Error> {
        Ok(0xa0_0000 + plan.thread_pointer_offset())
    }
}

struct Execution;

impl ExecutionImageBuilder<u64> for Execution {
    type Image = (GuestArchitecture, u64, u64, u64);

    fn build(
        &self,
        architecture: GuestArchitecture,
        loaded: &hl_loader::LoadedProcess,
        tls: &u64,
    ) -> Result<Self::Image, RuntimeExecError> {
        Ok((
            architecture,
            loaded.dynamic_handoff().start_entry(),
            loaded.initial_stack().stack_pointer(),
            *tls,
        ))
    }
}

struct Fixture;

impl Fixture {
    fn identity() -> (hl_task::ProcessId, hl_task::ThreadId) {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        tasks.create_init(credentials, ProcessLimits::empty()).unwrap()
    }

    fn plan() -> hl_linux::ExecPlan {
        hl_linux::ExecPlan {
            directory: None,
            path: b"/bin/program".to_vec(),
            arguments: vec![b"program".to_vec()],
            environment: vec![b"A=B".to_vec()],
            flags: 0,
        }
    }

    fn limits() -> LoadLimits {
        LoadLimits {
            stack_size: 0x20_000,
            pie_hint: Some(0x90_0000),
            interpreter_hint: Some(0x60_0000),
            stack_hint: Some(0x80_0000),
            ..LoadLimits::default()
        }
    }

    fn initial(
        architecture: GuestArchitecture,
    ) -> LoaderExecImage<AddressSpace, u64, (GuestArchitecture, u64, u64, u64)> {
        let source = Source {
            main: elf(architecture, false, false),
            interpreter: Vec::new(),
        };
        let mut loader = Loader::new(source, AddressSpace::new(None), Self::limits());
        let loaded = loader
            .load(LoadRequest {
                architecture,
                image_path: b"/old",
                executable_path: b"/old",
                arguments: &[b"old"],
                environment: &[],
                random: [1; 16],
                credentials: GuestCredentials::default(),
                features: GuestFeatures::default(),
            })
            .unwrap();
        let (_, address_space) = loader.into_parts();
        let tls = 0xa0_0000;
        let execution = (
            architecture,
            loaded.dynamic_handoff().start_entry(),
            loaded.initial_stack().stack_pointer(),
            tls,
        );
        LoaderExecImage {
            address_space,
            loaded,
            tls,
            execution,
        }
    }

    fn assert_old_image() {
        let participant = LoaderExecParticipant::new(
            GuestArchitecture::Aarch64,
            Self::limits(),
            Sources {
                architecture: GuestArchitecture::Aarch64,
                dynamic: true,
                malformed: true,
                malformed_interpreter: false,
                nested_interpreter: false,
            },
            Spaces(None),
            Arc::new(Context),
            Tls,
            Execution,
            Self::initial(GuestArchitecture::Aarch64),
        );
        let (process, thread) = Self::identity();
        let old = participant.current().1;
        assert!(participant.prepare(process, thread, &Self::plan()).is_err());
        assert!(Arc::ptr_eq(&participant.current().1, &old));
    }

    fn assert_address_image() {
        for failure in 1..40 {
            let participant = LoaderExecParticipant::new(
                GuestArchitecture::Aarch64,
                Self::limits(),
                Sources {
                    architecture: GuestArchitecture::Aarch64,
                    dynamic: true,
                    malformed: false,
                    malformed_interpreter: false,
                    nested_interpreter: false,
                },
                Spaces(Some(failure)),
                Arc::new(Context),
                Tls,
                Execution,
                Self::initial(GuestArchitecture::Aarch64),
            );
            let (process, thread) = Self::identity();
            let old = participant.current().1;
            if participant.prepare(process, thread, &Self::plan()).is_ok() {
                return;
            }
            assert!(Arc::ptr_eq(&participant.current().1, &old));
        }
        panic!("address-space failure sweep never reached a successful load");
    }
}

#[test]
fn isas_out_place() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        for dynamic in [false, true] {
            let participant = LoaderExecParticipant::new(
                architecture,
                Fixture::limits(),
                Sources {
                    architecture,
                    dynamic,
                    malformed: false,
                    malformed_interpreter: false,
                    nested_interpreter: false,
                },
                Spaces(None),
                Arc::new(Context),
                Tls,
                Execution,
                Fixture::initial(architecture),
            );
            let (process, _) = Fixture::identity();
            let old = participant.current().1;
            let mut prepared = participant.prepare_current(process, &Fixture::plan()).unwrap();
            let candidate = prepared.candidate().unwrap();
            assert!(Arc::ptr_eq(&participant.current().1, &old));
            prepared.publish().unwrap();
            let current = participant.current().1;
            assert!(Arc::ptr_eq(&candidate, &current));
            assert!(!Arc::ptr_eq(&current, &old));
            assert_eq!(current.loaded.interpreter().is_some(), dynamic);
            assert_eq!(current.execution.0, architecture);
            prepared.finish();
        }
    }
}

#[test]
fn malformed_old_image() {
    Fixture::assert_old_image();
    Fixture::assert_address_image();
}

#[derive(Default)]
struct Failures(Mutex<Vec<(hl_task::ProcessId, hl_loader::LoadError)>>);

impl LoadFailureReporter for Failures {
    fn report(&self, process: hl_task::ProcessId, error: hl_loader::LoadError) {
        self.0.lock().unwrap().push((process, error));
    }
}

#[test]
fn reports_precise_loader_failure_before_errno_projection() {
    for (malformed_interpreter, nested_interpreter, expected) in [
        (
            true,
            false,
            hl_loader::LoadError::Inspect {
                role: ImageRole::Interpreter,
                error: hl_loader::InspectError::InvalidMagic,
            },
        ),
        (false, true, hl_loader::LoadError::InvalidInterpreter),
    ] {
        let failures = Arc::new(Failures::default());
        let participant = LoaderExecParticipant::new(
            GuestArchitecture::Aarch64,
            Fixture::limits(),
            Sources {
                architecture: GuestArchitecture::Aarch64,
                dynamic: true,
                malformed: false,
                malformed_interpreter,
                nested_interpreter,
            },
            Spaces(None),
            Arc::new(Context),
            Tls,
            Execution,
            Fixture::initial(GuestArchitecture::Aarch64),
        )
        .with_failure_reporter(failures.clone());
        let (process, _) = Fixture::identity();

        assert_eq!(
            participant.prepare_current(process, &Fixture::plan()).err(),
            Some(RuntimeExecError::Format)
        );
        assert_eq!(*failures.0.lock().unwrap(), vec![(process, expected)]);
    }
}

#[path = "../exec/integration_test.rs"]
mod exec_integration_tests;
