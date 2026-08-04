use crate::{Action, BpfProgram, Data};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Disabled,
    Strict,
    Filter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Baseline {
    Container,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Status {
    pub mode: Mode,
    pub filters: usize,
}

impl Baseline {
    #[must_use]
    pub const fn status(self) -> Status {
        match self {
            Self::Container => Status {
                mode: Mode::Filter,
                filters: 1,
            },
            Self::Disabled => Status {
                mode: Mode::Disabled,
                filters: 0,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FilterInstallFlags {
    pub synchronize_threads: bool,
    pub log: bool,
    pub speculative_store_bypass_allowed: bool,
    pub new_listener: bool,
    pub synchronize_threads_esrch: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterInstallPlan {
    pub program: BpfProgram,
    pub flags: FilterInstallFlags,
    pub listener_requested: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyError {
    InvalidFlags,
    PermissionDenied,
    InvalidTransition,
    ListenerUnavailable,
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrapPlan {
    pub signal: u8,
    pub error: u16,
    pub code: i32,
    pub call_address: u64,
    pub syscall_number: i32,
    pub audit_architecture: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KillScope {
    Process,
    Thread,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Continue,
    ReturnErrno(u16),
    Trap(TrapPlan),
    Kill { scope: KillScope, signal: u8 },
    Trace { data: u16 },
    UserNotification { data: u16 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    mode: Mode,
    no_new_privileges: bool,
    filters: Vec<BpfProgram>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyImage {
    pub mode: Mode,
    pub no_new_privileges: bool,
    pub filters: Vec<Vec<crate::BpfInstruction>>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::Disabled,
            no_new_privileges: false,
            filters: Vec::new(),
        }
    }
}

impl Policy {
    #[must_use]
    pub fn checkpoint_image(&self) -> PolicyImage {
        PolicyImage {
            mode: self.mode,
            no_new_privileges: self.no_new_privileges,
            filters: self
                .filters
                .iter()
                .map(|program| program.instructions().to_vec())
                .collect(),
        }
    }

    pub fn restore_checkpoint(image: &PolicyImage) -> Result<Self, PolicyError> {
        if image.mode != Mode::Filter && !image.filters.is_empty()
            || image.mode == Mode::Filter && image.filters.is_empty()
        {
            return Err(PolicyError::InvalidTransition);
        }
        let filters = image
            .filters
            .iter()
            .cloned()
            .map(BpfProgram::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| PolicyError::InvalidTransition)?;
        let path_length = filters
            .iter()
            .map(|filter| filter.instructions().len() + 4)
            .sum::<usize>();
        if path_length > 32_768 {
            return Err(PolicyError::Capacity);
        }
        Ok(Self {
            mode: image.mode,
            no_new_privileges: image.no_new_privileges,
            filters,
        })
    }
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    #[must_use]
    pub const fn no_new_privileges(&self) -> bool {
        self.no_new_privileges
    }

    #[must_use]
    pub fn filter_count(&self) -> usize {
        self.filters.len()
    }

    #[must_use]
    pub fn same_filter_chain(&self, other: &Self) -> bool {
        self.mode == other.mode && self.filters == other.filters
    }

    pub fn enable_nnp(&mut self) {
        self.no_new_privileges = true;
    }

    pub fn strict(&mut self) -> Result<(), PolicyError> {
        if self.mode != Mode::Disabled {
            return Err(PolicyError::InvalidTransition);
        }
        self.mode = Mode::Strict;
        Ok(())
    }

    pub fn install_plan(program: BpfProgram, raw_flags: u32) -> Result<FilterInstallPlan, PolicyError> {
        if raw_flags & !0x1f != 0
            || raw_flags & 0x08 != 0 && raw_flags & 0x01 != 0
            || raw_flags & 0x10 != 0 && raw_flags & 0x01 == 0
        {
            return Err(PolicyError::InvalidFlags);
        }
        let flags = FilterInstallFlags {
            synchronize_threads: raw_flags & 0x01 != 0,
            log: raw_flags & 0x02 != 0,
            speculative_store_bypass_allowed: raw_flags & 0x04 != 0,
            new_listener: raw_flags & 0x08 != 0,
            synchronize_threads_esrch: raw_flags & 0x10 != 0,
        };
        Ok(FilterInstallPlan {
            program,
            flags,
            listener_requested: flags.new_listener,
        })
    }

    pub fn install(
        &mut self,
        plan: FilterInstallPlan,
        has_admin_capability: bool,
        listener_available: bool,
    ) -> Result<(), PolicyError> {
        if !self.no_new_privileges && !has_admin_capability {
            return Err(PolicyError::PermissionDenied);
        }
        if plan.listener_requested && !listener_available {
            return Err(PolicyError::ListenerUnavailable);
        }
        if self.mode == Mode::Strict {
            return Err(PolicyError::InvalidTransition);
        }
        let path_length = self
            .filters
            .iter()
            .map(|filter| filter.instructions().len() + 4)
            .sum::<usize>()
            + plan.program.instructions().len();
        if path_length > 32_768 {
            return Err(PolicyError::Capacity);
        }
        self.filters.insert(0, plan.program);
        self.mode = Mode::Filter;
        Ok(())
    }

    #[must_use]
    pub fn evaluate(&self, data: Data) -> Action {
        match self.mode {
            Mode::Disabled => Action::Allow { data: 0 },
            Mode::Strict => Self::strict_action(data),
            Mode::Filter => self.filter_action(data),
        }
    }

    #[must_use]
    pub fn decide(&self, data: Data) -> Decision {
        let action = self.evaluate(data);
        if self.mode == Mode::Strict {
            return match action {
                Action::Allow { .. } => Decision::Continue,
                _ => Decision::Kill {
                    scope: KillScope::Thread,
                    signal: 9,
                },
            };
        }
        match action {
            Action::Allow { .. } | Action::Log { .. } => Decision::Continue,
            Action::Errno { data } => Decision::ReturnErrno(data.min(4095)),
            Action::Trap { data: error } => Decision::Trap(TrapPlan {
                signal: 31,
                error,
                code: 1,
                call_address: data.instruction_pointer,
                syscall_number: data.number,
                audit_architecture: data.architecture,
            }),
            Action::KillProcess { .. } => Decision::Kill {
                scope: KillScope::Process,
                signal: 31,
            },
            Action::KillThread { .. } => Decision::Kill {
                scope: KillScope::Thread,
                signal: 31,
            },
            Action::Trace { data } => Decision::Trace { data },
            Action::UserNotification { data } => Decision::UserNotification { data },
        }
    }

    #[must_use]
    pub fn fork_snapshot(&self) -> Self {
        self.clone()
    }

    #[must_use]
    pub fn exec_snapshot(&self) -> Self {
        self.clone()
    }

    fn filter_action(&self, data: Data) -> Action {
        let mut best = Action::Allow { data: 0 };
        for program in &self.filters {
            let action = program.evaluate(data);
            if action.precedence() < best.precedence() {
                best = action;
            }
        }
        best
    }

    const fn strict_action(data: Data) -> Action {
        let allowed = if data.architecture == 0xc000_003e {
            matches!(data.number, 0 | 1 | 15 | 60)
        } else if data.architecture == 0xc000_00b7 {
            matches!(data.number, 63 | 64 | 93 | 139)
        } else {
            false
        };
        if allowed {
            Action::Allow { data: 0 }
        } else {
            Action::KillThread { data: 0 }
        }
    }
}
