use crate::{ImagePlan, ImageRole, InitialTlsPlan, LoadError, LoadedMapping, ReservedMapping};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadedModuleHandoff {
    pub role: ImageRole,
    pub mapping: LoadedMapping,
    pub load_bias: u64,
    pub entry: u64,
    pub tls_module_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicLoaderHandoff {
    start_entry: u64,
    main_entry: u64,
    interpreter_base: u64,
    modules: Vec<LoadedModuleHandoff>,
}

impl DynamicLoaderHandoff {
    pub(crate) fn build<R>(
        main_plan: &ImagePlan,
        main: &ReservedMapping<R>,
        main_bias: u64,
        interpreter_plan: Option<&ImagePlan>,
        interpreter: Option<&ReservedMapping<R>>,
        interpreter_bias: u64,
        tls: &InitialTlsPlan,
    ) -> Result<Self, LoadError> {
        let main_entry = main_plan
            .entry()
            .checked_add(main_bias)
            .ok_or(LoadError::InvalidReservation)?;
        let mut modules = vec![LoadedModuleHandoff {
            role: ImageRole::Main,
            mapping: LoadedMapping::from_reserved(main),
            load_bias: main_bias,
            entry: main_entry,
            tls_module_id: Self::tls_module_id(tls, ImageRole::Main),
        }];
        let (start_entry, interpreter_base) = match (interpreter_plan, interpreter) {
            (Some(plan), Some(mapping)) => {
                let entry = plan
                    .entry()
                    .checked_add(interpreter_bias)
                    .ok_or(LoadError::InvalidReservation)?;
                modules.push(LoadedModuleHandoff {
                    role: ImageRole::Interpreter,
                    mapping: LoadedMapping::from_reserved(mapping),
                    load_bias: interpreter_bias,
                    entry,
                    tls_module_id: Self::tls_module_id(tls, ImageRole::Interpreter),
                });
                (entry, interpreter_bias)
            }
            _ => (main_entry, 0),
        };
        Ok(Self {
            start_entry,
            main_entry,
            interpreter_base,
            modules,
        })
    }

    fn tls_module_id(tls: &InitialTlsPlan, role: ImageRole) -> Option<u32> {
        tls.modules()
            .iter()
            .find(|module| module.role() == role)
            .map(super::tls::TlsModulePlacement::module_id)
    }

    #[must_use]
    pub const fn start_entry(&self) -> u64 {
        self.start_entry
    }

    #[must_use]
    pub const fn main_entry(&self) -> u64 {
        self.main_entry
    }

    #[must_use]
    pub const fn interpreter_base(&self) -> u64 {
        self.interpreter_base
    }

    #[must_use]
    pub fn modules(&self) -> &[LoadedModuleHandoff] {
        &self.modules
    }
}
