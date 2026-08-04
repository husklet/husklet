use std::ops::Range;

use hl_isa::GuestArchitecture;

use crate::{ImageRole, TlsTemplate};

const TCB_SIZE: u64 = 64;
const DTV_HEADER_SIZE: u64 = 16;
const DTV_ENTRY_SIZE: u64 = 16;
const ABI_ALIGNMENT: u64 = 16;
const MODULE_MAXIMUM: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsVariant {
    /// Thread pointer precedes static TLS blocks.
    VariantOne,
    /// Static TLS blocks precede the thread pointer.
    VariantTwo,
}

#[derive(Clone, Copy, Debug)]
pub struct TlsModuleRequest<'template> {
    pub role: ImageRole,
    pub load_bias: u64,
    pub template: &'template TlsTemplate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsModulePlacement {
    module_id: u32,
    role: ImageRole,
    runtime_image_address: u64,
    storage: Range<u64>,
    template: TlsTemplate,
}

impl TlsModulePlacement {
    #[must_use]
    pub const fn module_id(&self) -> u32 {
        self.module_id
    }

    #[must_use]
    pub const fn role(&self) -> ImageRole {
        self.role
    }

    #[must_use]
    pub const fn runtime_image_address(&self) -> u64 {
        self.runtime_image_address
    }

    #[must_use]
    pub fn storage(&self) -> Range<u64> {
        self.storage.clone()
    }

    #[must_use]
    pub const fn template(&self) -> &TlsTemplate {
        &self.template
    }
}

/// Value-only initial-thread TLS allocation layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitialTlsPlan {
    architecture: GuestArchitecture,
    variant: TlsVariant,
    allocation_size: u64,
    allocation_alignment: u64,
    thread_pointer_offset: u64,
    tcb: Range<u64>,
    dtv: Range<u64>,
    modules: Vec<TlsModulePlacement>,
}

impl InitialTlsPlan {
    pub fn build(architecture: GuestArchitecture, modules: &[TlsModuleRequest<'_>]) -> Result<Self, TlsPlanError> {
        if modules.len() > MODULE_MAXIMUM {
            return Err(TlsPlanError::TooManyModules);
        }
        let alignment = modules
            .iter()
            .map(|module| module.template.alignment())
            .max()
            .unwrap_or(1)
            .max(ABI_ALIGNMENT);
        let variant = match architecture {
            GuestArchitecture::Aarch64 => TlsVariant::VariantOne,
            GuestArchitecture::X86_64 => TlsVariant::VariantTwo,
        };
        match variant {
            TlsVariant::VariantOne => Self::variant_one(architecture, modules, alignment),
            TlsVariant::VariantTwo => Self::variant_two(architecture, modules, alignment),
        }
    }

    fn variant_one(
        architecture: GuestArchitecture,
        requests: &[TlsModuleRequest<'_>],
        alignment: u64,
    ) -> Result<Self, TlsPlanError> {
        let tcb = 0..TCB_SIZE;
        let mut cursor = TCB_SIZE;
        let modules = Self::place_modules(requests, &mut cursor)?;
        cursor = Self::align_up(cursor, ABI_ALIGNMENT)?;
        let dtv_size = Self::dtv_size(modules.len())?;
        let dtv = cursor..cursor.checked_add(dtv_size).ok_or(TlsPlanError::Overflow)?;
        let allocation_size = Self::align_up(dtv.end, alignment)?;
        Ok(Self {
            architecture,
            variant: TlsVariant::VariantOne,
            allocation_size,
            allocation_alignment: alignment,
            thread_pointer_offset: tcb.start,
            tcb,
            dtv,
            modules,
        })
    }

    fn variant_two(
        architecture: GuestArchitecture,
        requests: &[TlsModuleRequest<'_>],
        alignment: u64,
    ) -> Result<Self, TlsPlanError> {
        let mut cursor = 0;
        let modules = Self::place_modules(requests, &mut cursor)?;
        cursor = Self::align_up(cursor, ABI_ALIGNMENT)?;
        let tcb = cursor..cursor.checked_add(TCB_SIZE).ok_or(TlsPlanError::Overflow)?;
        cursor = tcb.end;
        let dtv_size = Self::dtv_size(modules.len())?;
        let dtv = cursor..cursor.checked_add(dtv_size).ok_or(TlsPlanError::Overflow)?;
        let allocation_size = Self::align_up(dtv.end, alignment)?;
        Ok(Self {
            architecture,
            variant: TlsVariant::VariantTwo,
            allocation_size,
            allocation_alignment: alignment,
            thread_pointer_offset: tcb.start,
            tcb,
            dtv,
            modules,
        })
    }

    fn place_modules(
        requests: &[TlsModuleRequest<'_>],
        cursor: &mut u64,
    ) -> Result<Vec<TlsModulePlacement>, TlsPlanError> {
        let mut modules = Vec::with_capacity(requests.len());
        for (index, request) in requests.iter().enumerate() {
            *cursor = Self::align_up(*cursor, request.template.alignment())?;
            let end = cursor
                .checked_add(request.template.memory_size())
                .ok_or(TlsPlanError::Overflow)?;
            let runtime_image_address = request
                .template
                .link_address()
                .checked_add(request.load_bias)
                .ok_or(TlsPlanError::Overflow)?;
            modules.push(TlsModulePlacement {
                module_id: u32::try_from(index + 1).map_err(|_| TlsPlanError::TooManyModules)?,
                role: request.role,
                runtime_image_address,
                storage: *cursor..end,
                template: request.template.clone(),
            });
            *cursor = end;
        }
        Ok(modules)
    }

    fn dtv_size(module_count: usize) -> Result<u64, TlsPlanError> {
        let entries = u64::try_from(module_count).map_err(|_| TlsPlanError::TooManyModules)?;
        DTV_HEADER_SIZE
            .checked_add(entries.checked_mul(DTV_ENTRY_SIZE).ok_or(TlsPlanError::Overflow)?)
            .ok_or(TlsPlanError::Overflow)
    }

    fn align_up(value: u64, alignment: u64) -> Result<u64, TlsPlanError> {
        value
            .checked_add(alignment - 1)
            .map(|value| value & !(alignment - 1))
            .ok_or(TlsPlanError::Overflow)
    }

    #[must_use]
    pub const fn architecture(&self) -> GuestArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn variant(&self) -> TlsVariant {
        self.variant
    }

    #[must_use]
    pub const fn allocation_size(&self) -> u64 {
        self.allocation_size
    }

    #[must_use]
    pub const fn allocation_alignment(&self) -> u64 {
        self.allocation_alignment
    }

    #[must_use]
    pub const fn thread_pointer_offset(&self) -> u64 {
        self.thread_pointer_offset
    }

    #[must_use]
    pub fn tcb(&self) -> Range<u64> {
        self.tcb.clone()
    }

    #[must_use]
    pub fn dtv(&self) -> Range<u64> {
        self.dtv.clone()
    }

    #[must_use]
    pub fn modules(&self) -> &[TlsModulePlacement] {
        &self.modules
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsPlanError {
    TooManyModules,
    Overflow,
}

/// Later task/execution adapters implement allocation and architecture register
/// installation from the immutable plan. The loader never allocates TLS.
pub trait ThreadLocalStorage {
    type Prepared;
    type Error;

    fn prepare_initial(&mut self, plan: &InitialTlsPlan) -> Result<Self::Prepared, Self::Error>;
}
