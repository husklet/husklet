use crate::{
    AddressSpaceError, DynamicLoaderHandoff, ElfInspector, ExecutablePlacement, GuestCredentials, GuestFeatures,
    GuestProtectionPlan, ImageKind, ImagePlan, ImageProjection, ImageProtectionPlan, ImageProtectionRegistry,
    ImageRole, ImageSource, ImageSourceError, InitialStack, InitialTlsPlan, InspectError, LoadLimits, MappingKind,
    MappingPlacement, Protection, ProtectionPlanError, ReservedMapping, StackError, StackPlanner, StackRequest,
    TlsModuleRequest, TlsPlanError, TransactionalAddressSpace, mapping_transaction::MappingTransaction,
};
use hl_isa::GuestArchitecture;
use std::{error::Error, fmt};

/// Complete host-neutral input to one process-image transaction.
pub struct LoadRequest<'a> {
    pub architecture: GuestArchitecture,
    /// Object read as the main ELF image. This differs from
    /// `executable_path` when Linux resolves a `#!` script.
    pub image_path: &'a [u8],
    /// Original exec filename projected through `AT_EXECFN`.
    pub executable_path: &'a [u8],
    pub arguments: &'a [&'a [u8]],
    pub environment: &'a [&'a [u8]],
    pub random: [u8; 16],
    pub credentials: GuestCredentials,
    pub features: GuestFeatures,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadError {
    Source { role: ImageRole, error: ImageSourceError },
    Inspect { role: ImageRole, error: InspectError },
    AddressSpace(AddressSpaceError),
    Stack(StackError),
    InvalidReservation,
    InvalidInterpreter,
    Tls(TlsPlanError),
    Protection(ProtectionPlanError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "load transaction failed: {self:?}")
    }
}

impl Error for LoadError {}

/// Published mapping coordinates, without adapter reservation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadedMapping {
    address: u64,
    size: u64,
}

impl LoadedMapping {
    pub(crate) const fn from_reserved<R>(mapping: &ReservedMapping<R>) -> Self {
        Self {
            address: mapping.address(),
            size: mapping.size(),
        }
    }

    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    #[must_use]
    pub const fn size(self) -> u64 {
        self.size
    }
}

/// Result returned only after every mapping becomes visible atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProcess {
    main: LoadedMapping,
    interpreter: Option<LoadedMapping>,
    stack_mapping: LoadedMapping,
    usable_stack: LoadedMapping,
    stack_overread: Option<LoadedMapping>,
    initial_stack: InitialStack,
    tls: InitialTlsPlan,
    dynamic_handoff: DynamicLoaderHandoff,
    main_projection: Option<ImageProjection>,
}

impl LoadedProcess {
    #[must_use]
    pub const fn main(&self) -> LoadedMapping {
        self.main
    }

    #[must_use]
    pub const fn interpreter(&self) -> Option<LoadedMapping> {
        self.interpreter
    }

    #[must_use]
    pub const fn stack_mapping(&self) -> LoadedMapping {
        self.stack_mapping
    }

    /// Guest-visible writable main-stack interval, excluding its lower guard.
    #[must_use]
    pub const fn usable_stack(&self) -> LoadedMapping {
        self.usable_stack
    }

    /// Writable cushion above the logical stack top on x86-64.
    #[must_use]
    pub const fn stack_overread(&self) -> Option<LoadedMapping> {
        self.stack_overread
    }

    #[must_use]
    pub const fn initial_stack(&self) -> &InitialStack {
        &self.initial_stack
    }

    #[must_use]
    pub const fn initial_tls(&self) -> &InitialTlsPlan {
        &self.tls
    }

    #[must_use]
    pub const fn dynamic_handoff(&self) -> &DynamicLoaderHandoff {
        &self.dynamic_handoff
    }

    #[must_use]
    pub const fn main_projection(&self) -> Option<ImageProjection> {
        self.main_projection
    }
}

/// Coordinates bounded source reads and one atomic address-space publication.
pub struct Loader<S, A> {
    source: S,
    address_space: A,
    limits: LoadLimits,
}

impl<S, A> Loader<S, A>
where
    S: ImageSource,
    A: TransactionalAddressSpace + ImageProtectionRegistry<A::Reservation>,
{
    #[must_use]
    pub const fn new(source: S, address_space: A, limits: LoadLimits) -> Self {
        Self {
            source,
            address_space,
            limits,
        }
    }

    pub fn load(&mut self, request: LoadRequest<'_>) -> Result<LoadedProcess, LoadError> {
        let main_bytes = self.read(ImageRole::Main, request.image_path)?;
        let main_plan = self.inspect(ImageRole::Main, request.architecture, &main_bytes)?;
        let interpreter_image = self.prepare_interpreter(request.architecture, &main_plan)?;
        let interpreter_plan = interpreter_image.as_ref().map(|(_, plan)| plan.clone());
        let mut transaction = MappingTransaction::new(&mut self.address_space);
        let main = Self::reserve_main(
            &mut transaction,
            &main_plan,
            self.limits.executable_placement,
            self.limits.pie_hint,
        )?;
        Self::validate_reservation(&main, main_plan.image_span())?;
        Self::stage_image(
            &mut transaction,
            &main,
            &main_plan,
            &main_bytes,
            self.limits.host_page_size,
        )?;

        let interpreter = if let Some((bytes, plan)) = interpreter_image {
            let mapping = transaction.reserve(
                MappingKind::Interpreter,
                plan.image_span(),
                MappingPlacement::Hint(self.limits.interpreter_hint),
            )?;
            Self::validate_reservation(&mapping, plan.image_span())?;
            Self::stage_image(&mut transaction, &mapping, &plan, &bytes, self.limits.host_page_size)?;
            Some(mapping)
        } else {
            None
        };
        let stack_overread_size = match request.architecture {
            GuestArchitecture::Aarch64 => 0,
            GuestArchitecture::X86_64 => self.limits.x86_stack_overread_size,
        };
        let stack_reservation_size = self
            .limits
            .stack_guard_size
            .checked_add(self.limits.stack_size)
            .and_then(|size| size.checked_add(stack_overread_size))
            .ok_or(LoadError::InvalidReservation)?;
        // `stack_hint` predates the lower guard and names the usable stack
        // base. Keep that guest coordinate stable by placing the newly owned
        // guard immediately below it.
        let stack_reservation_hint = match self.limits.stack_hint {
            Some(address) => Some(
                address
                    .checked_sub(self.limits.stack_guard_size)
                    .ok_or(LoadError::InvalidReservation)?,
            ),
            None => None,
        };
        let stack_mapping = transaction.reserve(
            MappingKind::Stack,
            stack_reservation_size,
            MappingPlacement::Hint(stack_reservation_hint),
        )?;
        Self::validate_reservation(&stack_mapping, stack_reservation_size)?;
        let stack_top = stack_mapping
            .address()
            .checked_add(self.limits.stack_guard_size)
            .and_then(|address| address.checked_add(self.limits.stack_size))
            .ok_or(LoadError::InvalidReservation)?;
        let main_bias = Self::guest_bias(&main_plan, &main)?;
        let interpreter_bias = match (&interpreter_plan, &interpreter) {
            (Some(plan), Some(mapping)) => Self::guest_bias(plan, mapping)?,
            _ => 0,
        };
        let tls = Self::plan_tls(
            request.architecture,
            &main_plan,
            main_bias,
            interpreter_plan.as_ref(),
            interpreter_bias,
        )?;
        let dynamic_handoff = DynamicLoaderHandoff::build(
            &main_plan,
            &main,
            main_bias,
            interpreter_plan.as_ref(),
            interpreter.as_ref(),
            interpreter_bias,
            &tls,
        )?;
        let main_projection = ImageProjection::build(&main_plan, &main)?;
        let initial_stack = StackPlanner::new(self.limits.stack).plan(StackRequest {
            image: &main_plan,
            load_bias: main_bias,
            interpreter_base: interpreter_bias,
            stack_top,
            arguments: request.arguments,
            environment: request.environment,
            executable_path: request.executable_path,
            random: request.random,
            credentials: request.credentials,
            features: request.features,
        })?;
        let stack_offset = initial_stack
            .stack_pointer()
            .checked_sub(stack_mapping.address())
            .ok_or(LoadError::InvalidReservation)?;
        if stack_offset < self.limits.stack_guard_size {
            return Err(LoadError::InvalidReservation);
        }
        transaction.stage_write(&stack_mapping, stack_offset, initial_stack.bytes())?;
        if self.limits.stack_guard_size != 0 {
            transaction.stage_protection(
                &stack_mapping,
                0,
                self.limits.stack_guard_size,
                Protection::from_bits(0),
            )?;
        }
        transaction.stage_protection(
            &stack_mapping,
            self.limits.stack_guard_size,
            self.limits
                .stack_size
                .checked_add(stack_overread_size)
                .ok_or(LoadError::InvalidReservation)?,
            Protection::from_bits(Protection::READ | Protection::WRITE),
        )?;
        let usable_stack = LoadedMapping {
            address: stack_mapping
                .address()
                .checked_add(self.limits.stack_guard_size)
                .ok_or(LoadError::InvalidReservation)?,
            size: self.limits.stack_size,
        };
        let stack_overread = if stack_overread_size == 0 {
            None
        } else {
            Some(LoadedMapping {
                address: stack_top,
                size: stack_overread_size,
            })
        };
        transaction.commit()?;
        Ok(LoadedProcess {
            main: LoadedMapping::from_reserved(&main),
            interpreter: interpreter.as_ref().map(LoadedMapping::from_reserved),
            stack_mapping: LoadedMapping::from_reserved(&stack_mapping),
            usable_stack,
            stack_overread,
            initial_stack,
            tls,
            dynamic_handoff,
            main_projection,
        })
    }

    fn read(&mut self, role: ImageRole, path: &[u8]) -> Result<Vec<u8>, LoadError> {
        self.source
            .read_image(role, path, self.limits.image.max_image_bytes)
            .map_err(|error| LoadError::Source { role, error })
    }

    fn inspect(&self, role: ImageRole, architecture: GuestArchitecture, bytes: &[u8]) -> Result<ImagePlan, LoadError> {
        ElfInspector::new(architecture, self.limits.image)
            .inspect(bytes)
            .map_err(|error| LoadError::Inspect { role, error })
    }

    fn reserve_main(
        transaction: &mut MappingTransaction<'_, A>,
        plan: &ImagePlan,
        placement: ExecutablePlacement,
        pie_hint: Option<u64>,
    ) -> Result<ReservedMapping<A::Reservation>, LoadError> {
        if plan.kind() == ImageKind::PositionIndependent {
            return Ok(transaction.reserve(
                MappingKind::MainImage,
                plan.image_span(),
                MappingPlacement::Hint(pie_hint),
            )?);
        }
        let mapping = match placement {
            ExecutablePlacement::FixedLink => transaction.reserve(
                MappingKind::MainImage,
                plan.image_span(),
                MappingPlacement::Fixed(plan.link_base()),
            )?,
            ExecutablePlacement::PreferLink { fallback_hint } => {
                match transaction.reserve(
                    MappingKind::MainImage,
                    plan.image_span(),
                    MappingPlacement::Fixed(plan.link_base()),
                ) {
                    Ok(mapping) => mapping,
                    Err(AddressSpaceError::Conflict) => transaction.reserve(
                        MappingKind::MainImage,
                        plan.image_span(),
                        MappingPlacement::Hint(fallback_hint),
                    )?,
                    Err(error) => return Err(error.into()),
                }
            }
            ExecutablePlacement::Rebased { deterministic_hint } => transaction.reserve(
                MappingKind::MainImage,
                plan.image_span(),
                MappingPlacement::Hint(deterministic_hint),
            )?,
        };
        if matches!(placement, ExecutablePlacement::FixedLink) && mapping.address() != plan.link_base() {
            return Err(LoadError::InvalidReservation);
        }
        Ok(mapping)
    }

    fn prepare_interpreter(
        &mut self,
        architecture: GuestArchitecture,
        main: &ImagePlan,
    ) -> Result<Option<(Vec<u8>, ImagePlan)>, LoadError> {
        let Some(path) = main.interpreter() else {
            return Ok(None);
        };
        let bytes = self.read(ImageRole::Interpreter, path.as_bytes())?;
        let plan = self.inspect(ImageRole::Interpreter, architecture, &bytes)?;
        if plan.kind() != ImageKind::PositionIndependent || plan.interpreter().is_some() {
            return Err(LoadError::InvalidInterpreter);
        }
        Ok(Some((bytes, plan)))
    }

    fn validate_reservation<R>(mapping: &ReservedMapping<R>, expected_size: u64) -> Result<(), LoadError> {
        if mapping.size() != expected_size || mapping.address().checked_add(mapping.size()).is_none() {
            return Err(LoadError::InvalidReservation);
        }
        Ok(())
    }

    fn guest_bias<R>(plan: &ImagePlan, mapping: &ReservedMapping<R>) -> Result<u64, LoadError> {
        if plan.kind() == ImageKind::Executable {
            return Ok(0);
        }
        mapping
            .address()
            .checked_sub(plan.link_base())
            .ok_or(LoadError::InvalidReservation)
    }

    fn stage_image(
        transaction: &mut MappingTransaction<'_, A>,
        mapping: &ReservedMapping<A::Reservation>,
        plan: &ImagePlan,
        image: &[u8],
        host_page_size: u64,
    ) -> Result<(), LoadError> {
        transaction.stage_executable(mapping)?;
        for segment in plan.segments() {
            let offset = segment
                .guest_address()
                .checked_sub(plan.link_base())
                .ok_or(LoadError::InvalidReservation)?;
            let source = segment.source().as_range();
            // A loadable segment may be pure BSS (`p_filesz == 0`).  Such a
            // segment has no file write to stage; its entire memory extent is
            // supplied by the zero-fill operation below.  Keeping zero-length
            // writes out of the address-space port also preserves its useful
            // invariant that every staged mutation covers a non-empty range.
            if !source.is_empty() {
                transaction.stage_write(mapping, offset, &image[source])?;
            }
            if segment.zero_fill_size() != 0 {
                let zero_offset = offset
                    .checked_add(segment.source().size())
                    .ok_or(LoadError::InvalidReservation)?;
                transaction.stage_zero(mapping, zero_offset, segment.zero_fill_size())?;
            }
        }
        let protections = ImageProtectionPlan::build(plan, host_page_size).map_err(LoadError::Protection)?;
        for range in protections.ranges() {
            transaction.stage_protection(mapping, range.mapping_offset(), range.size(), range.protection())?;
        }
        let guest_base = if plan.kind() == ImageKind::Executable {
            plan.link_base()
        } else {
            mapping.address()
        };
        let guest_protections = GuestProtectionPlan::build(plan, guest_base).map_err(LoadError::Protection)?;
        for range in guest_protections.ranges() {
            transaction.stage_guest_access(mapping, range.guest_address, range.size, range.read_only)?;
        }
        Ok(())
    }

    fn plan_tls(
        architecture: GuestArchitecture,
        main: &ImagePlan,
        main_bias: u64,
        interpreter: Option<&ImagePlan>,
        interpreter_bias: u64,
    ) -> Result<InitialTlsPlan, LoadError> {
        let mut modules = Vec::with_capacity(2);
        if let Some(template) = main.tls() {
            modules.push(TlsModuleRequest {
                role: ImageRole::Main,
                load_bias: main_bias,
                template,
            });
        }
        if let Some(template) = interpreter.and_then(ImagePlan::tls) {
            modules.push(TlsModuleRequest {
                role: ImageRole::Interpreter,
                load_bias: interpreter_bias,
                template,
            });
        }
        InitialTlsPlan::build(architecture, &modules).map_err(LoadError::Tls)
    }

    #[must_use]
    pub fn into_parts(self) -> (S, A) {
        (self.source, self.address_space)
    }
}

impl From<AddressSpaceError> for LoadError {
    fn from(error: AddressSpaceError) -> Self {
        Self::AddressSpace(error)
    }
}

impl From<StackError> for LoadError {
    fn from(error: StackError) -> Self {
        Self::Stack(error)
    }
}
