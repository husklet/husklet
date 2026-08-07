use std::{error::Error, fmt};

use hl_isa::GuestArchitecture;

use crate::stack_layout::StackLayout;
use crate::{ImageKind, ImagePlan};

const RANDOM_BYTES: usize = 16;

/// Bounds applied before retaining guest argument or environment bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackLimits {
    pub max_arguments: usize,
    pub max_environment: usize,
    pub max_string_bytes: usize,
    pub max_single_string_bytes: usize,
    pub max_stack_image_bytes: usize,
}

impl Default for StackLimits {
    fn default() -> Self {
        Self {
            max_arguments: 4096,
            max_environment: 4096,
            max_string_bytes: 2 * 1024 * 1024,
            max_single_string_bytes: 128 * 1024,
            max_stack_image_bytes: 2 * 1024 * 1024 + 128 * 1024,
        }
    }
}

/// Guest identity values advertised in the initial auxiliary vector.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GuestCredentials {
    pub user: u32,
    pub effective_user: u32,
    pub group: u32,
    pub effective_group: u32,
}

/// Architecture feature masks advertised to the guest dynamic loader.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GuestFeatures {
    pub hardware: u64,
    pub hardware_second: u64,
}

/// Inputs required to produce a guest-visible initial process stack.
pub struct StackRequest<'a> {
    pub image: &'a ImagePlan,
    pub load_bias: u64,
    pub interpreter_base: u64,
    pub stack_top: u64,
    pub arguments: &'a [&'a [u8]],
    pub environment: &'a [&'a [u8]],
    pub executable_path: &'a [u8],
    pub random: [u8; RANDOM_BYTES],
    pub credentials: GuestCredentials,
    pub features: GuestFeatures,
}

/// Linux auxiliary-vector identifiers emitted by the planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
pub enum AuxiliaryType {
    Null = 0,
    ProgramHeaders = 3,
    ProgramHeaderSize = 4,
    ProgramHeaderCount = 5,
    PageSize = 6,
    InterpreterBase = 7,
    Flags = 8,
    Entry = 9,
    User = 11,
    EffectiveUser = 12,
    Group = 13,
    EffectiveGroup = 14,
    Platform = 15,
    HardwareCapabilities = 16,
    ClockTicks = 17,
    Secure = 23,
    Random = 25,
    HardwareCapabilitiesSecond = 26,
    ExecutablePath = 31,
}

/// One key/value pair in the guest auxiliary vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuxiliaryEntry {
    kind: AuxiliaryType,
    value: u64,
}

impl AuxiliaryEntry {
    const fn new(kind: AuxiliaryType, value: u64) -> Self {
        Self { kind, value }
    }

    #[must_use]
    pub const fn kind(self) -> AuxiliaryType {
        self.kind
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }
}

/// Contiguous, immutable write plan for the initial guest stack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitialStack {
    address: u64,
    bytes: Vec<u8>,
    argument_addresses: Vec<u64>,
    environment_addresses: Vec<u64>,
    auxiliary: Vec<AuxiliaryEntry>,
}

impl InitialStack {
    #[must_use]
    pub const fn stack_pointer(&self) -> u64 {
        self.address
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn argument_addresses(&self) -> &[u64] {
        &self.argument_addresses
    }

    #[must_use]
    pub fn environment_addresses(&self) -> &[u64] {
        &self.environment_addresses
    }

    #[must_use]
    pub fn auxiliary(&self) -> &[AuxiliaryEntry] {
        &self.auxiliary
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackError {
    TooManyArguments,
    TooManyEnvironmentEntries,
    EmptyExecutablePath,
    EmbeddedNul,
    StringTooLong,
    StringsTooLarge,
    ExecutableBias,
    MissingProgramHeaderAddress,
    AddressOverflow,
    StackImageTooLarge,
}

impl fmt::Display for StackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid initial stack request: {self:?}")
    }
}

impl Error for StackError {}

/// Stateless initial-stack planner.
#[derive(Clone, Copy, Debug)]
pub struct StackPlanner {
    limits: StackLimits,
}

impl StackPlanner {
    #[must_use]
    pub const fn new(limits: StackLimits) -> Self {
        Self { limits }
    }

    // Consumes the planner so one plan cannot be built twice from the same request.
    #[allow(clippy::needless_pass_by_value)]
    pub fn plan(self, request: StackRequest<'_>) -> Result<InitialStack, StackError> {
        self.validate_counts(&request)?;
        self.validate_strings(&request)?;
        let (entry, program_headers) = Self::runtime_image_values(&request)?;
        let write_capacity = request
            .arguments
            .len()
            .checked_add(request.environment.len())
            .and_then(|count| count.checked_add(3))
            .ok_or(StackError::StackImageTooLarge)?;
        let mut layout = StackLayout::new(request.stack_top, self.limits.max_stack_image_bytes, write_capacity);
        let string_order = StackStringOrder::for_architecture(request.image.architecture());
        let (argument_addresses, environment_addresses) = match string_order {
            StackStringOrder::KernelAddressOrder => {
                let environment = layout.place_address_order(request.environment)?;
                let arguments = layout.place_address_order(request.arguments)?;
                (arguments, environment)
            }
            StackStringOrder::ForwardTopDown => {
                let arguments = layout.place_top_down(request.arguments)?;
                let environment = layout.place_top_down(request.environment)?;
                (arguments, environment)
            }
        };
        let executable_path = layout.place_nul_terminated(request.executable_path)?;
        let platform = layout.place_bytes(Self::platform_storage(request.image.architecture()))?;
        let random = layout.place_bytes(&request.random)?;
        layout.align_cursor()?;
        let auxiliary = Self::auxiliary(&request, program_headers, entry, platform, random, executable_path);
        let stack_pointer =
            layout.reserve_table(argument_addresses.len(), environment_addresses.len(), auxiliary.len())?;
        let mut bytes = layout.materialize(stack_pointer)?;
        Self::write_table(
            &mut bytes,
            argument_addresses.as_slice(),
            environment_addresses.as_slice(),
            auxiliary.as_slice(),
        );
        Ok(InitialStack {
            address: stack_pointer,
            bytes,
            argument_addresses,
            environment_addresses,
            auxiliary,
        })
    }

    fn validate_counts(self, request: &StackRequest<'_>) -> Result<(), StackError> {
        if request.arguments.len() > self.limits.max_arguments {
            return Err(StackError::TooManyArguments);
        }
        if request.environment.len() > self.limits.max_environment {
            return Err(StackError::TooManyEnvironmentEntries);
        }
        if request.executable_path.is_empty() {
            return Err(StackError::EmptyExecutablePath);
        }
        Ok(())
    }

    fn validate_strings(self, request: &StackRequest<'_>) -> Result<(), StackError> {
        let strings = request
            .arguments
            .iter()
            .chain(request.environment)
            .copied()
            .chain([request.executable_path]);
        let mut total = 0_usize;
        for value in strings {
            if value.contains(&0) {
                return Err(StackError::EmbeddedNul);
            }
            if value.len() >= self.limits.max_single_string_bytes {
                return Err(StackError::StringTooLong);
            }
            total = total.checked_add(value.len() + 1).ok_or(StackError::StringsTooLarge)?;
            if total > self.limits.max_string_bytes {
                return Err(StackError::StringsTooLarge);
            }
        }
        Ok(())
    }

    fn runtime_image_values(request: &StackRequest<'_>) -> Result<(u64, u64), StackError> {
        if request.image.kind() == ImageKind::Executable && request.load_bias != 0 {
            return Err(StackError::ExecutableBias);
        }
        let bias = if request.image.kind() == ImageKind::PositionIndependent {
            request.load_bias
        } else {
            0
        };
        let entry = request
            .image
            .entry()
            .checked_add(bias)
            .ok_or(StackError::AddressOverflow)?;
        let program_headers = request
            .image
            .program_headers()
            .guest_address()
            .ok_or(StackError::MissingProgramHeaderAddress)?
            .checked_add(bias)
            .ok_or(StackError::AddressOverflow)?;
        Ok((entry, program_headers))
    }

    const fn platform_storage(architecture: GuestArchitecture) -> &'static [u8; 8] {
        match architecture {
            GuestArchitecture::Aarch64 => b"aarch64\0",
            GuestArchitecture::X86_64 => b"x86_64\0\0",
        }
    }

    fn auxiliary(
        request: &StackRequest<'_>,
        program_headers: u64,
        entry: u64,
        platform: u64,
        random: u64,
        executable_path: u64,
    ) -> Vec<AuxiliaryEntry> {
        let headers = request.image.program_headers();
        let credentials = request.credentials;
        let features = request.features;
        let mut entries = vec![
            AuxiliaryEntry::new(AuxiliaryType::ProgramHeaders, program_headers),
            AuxiliaryEntry::new(AuxiliaryType::ProgramHeaderSize, u64::from(headers.entry_size())),
            AuxiliaryEntry::new(AuxiliaryType::ProgramHeaderCount, u64::from(headers.entry_count())),
            AuxiliaryEntry::new(AuxiliaryType::PageSize, 4096),
            AuxiliaryEntry::new(AuxiliaryType::InterpreterBase, request.interpreter_base),
            AuxiliaryEntry::new(AuxiliaryType::Flags, 0),
            AuxiliaryEntry::new(AuxiliaryType::Entry, entry),
            AuxiliaryEntry::new(AuxiliaryType::User, u64::from(credentials.user)),
            AuxiliaryEntry::new(AuxiliaryType::EffectiveUser, u64::from(credentials.effective_user)),
            AuxiliaryEntry::new(AuxiliaryType::Group, u64::from(credentials.group)),
            AuxiliaryEntry::new(AuxiliaryType::EffectiveGroup, u64::from(credentials.effective_group)),
            AuxiliaryEntry::new(AuxiliaryType::HardwareCapabilities, features.hardware),
        ];
        Self::append_architecture_auxiliary(
            &mut entries,
            request.image.architecture(),
            features.hardware_second,
            platform,
            random,
        );
        entries.extend([
            AuxiliaryEntry::new(AuxiliaryType::ExecutablePath, executable_path),
            AuxiliaryEntry::new(AuxiliaryType::Null, 0),
        ]);
        entries
    }

    fn append_architecture_auxiliary(
        entries: &mut Vec<AuxiliaryEntry>,
        architecture: GuestArchitecture,
        hardware_second: u64,
        platform: u64,
        random: u64,
    ) {
        if architecture == GuestArchitecture::Aarch64 {
            entries.extend([
                AuxiliaryEntry::new(AuxiliaryType::HardwareCapabilitiesSecond, hardware_second),
                AuxiliaryEntry::new(AuxiliaryType::ClockTicks, 100),
                AuxiliaryEntry::new(AuxiliaryType::Platform, platform),
                AuxiliaryEntry::new(AuxiliaryType::Random, random),
                AuxiliaryEntry::new(AuxiliaryType::Secure, 0),
            ]);
        } else {
            entries.extend([
                AuxiliaryEntry::new(AuxiliaryType::Platform, platform),
                AuxiliaryEntry::new(AuxiliaryType::Random, random),
                AuxiliaryEntry::new(AuxiliaryType::Secure, 0),
                AuxiliaryEntry::new(AuxiliaryType::ClockTicks, 100),
                AuxiliaryEntry::new(AuxiliaryType::HardwareCapabilitiesSecond, hardware_second),
            ]);
        }
    }

    fn write_table(output: &mut [u8], arguments: &[u64], environment: &[u64], auxiliary: &[AuxiliaryEntry]) {
        let mut offset = 0;
        Self::write_word(output, &mut offset, arguments.len() as u64);
        for address in arguments {
            Self::write_word(output, &mut offset, *address);
        }
        Self::write_word(output, &mut offset, 0);
        for address in environment {
            Self::write_word(output, &mut offset, *address);
        }
        Self::write_word(output, &mut offset, 0);
        for entry in auxiliary {
            Self::write_word(output, &mut offset, entry.kind as u64);
            Self::write_word(output, &mut offset, entry.value);
        }
    }

    fn write_word(output: &mut [u8], offset: &mut usize, value: u64) {
        output[*offset..*offset + 8].copy_from_slice(&value.to_le_bytes());
        *offset += 8;
    }
}

/// The two C loaders currently disagree on string placement.
///
/// `AArch64` carries the corrected Linux ascending-address order. The x86-64
/// oracle still places each list forward while moving the cursor downward.
/// Preserve both until a native-Linux oracle establishes an intentional
/// migration away from the x86-64 baseline.
#[derive(Clone, Copy)]
enum StackStringOrder {
    KernelAddressOrder,
    ForwardTopDown,
}

impl StackStringOrder {
    const fn for_architecture(architecture: GuestArchitecture) -> Self {
        match architecture {
            GuestArchitecture::Aarch64 => Self::KernelAddressOrder,
            GuestArchitecture::X86_64 => Self::ForwardTopDown,
        }
    }
}
