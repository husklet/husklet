use hl_linux::{Errno, GuestMemory, LinuxResult, PrctlPlan};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn credential_prctl(&self, plan: PrctlPlan) -> Option<LinuxResult> {
        let mut credentials = self.snapshot().ok()?.credentials;
        match plan {
            PrctlPlan::GetNoNewPrivileges => {
                return Some(LinuxResult::Value(u64::from(credentials.no_new_privileges)));
            }
            PrctlPlan::SetNoNewPrivileges => credentials.no_new_privileges = true,
            PrctlPlan::GetKeepCapabilities => {
                return Some(LinuxResult::Value(u64::from(credentials.keep_capabilities)));
            }
            PrctlPlan::SetKeepCapabilities(value) => {
                if !credentials.set_keep_capabilities(value) {
                    return Some(LinuxResult::Error(Errno::EPERM));
                }
            }
            PrctlPlan::ReadCapability(capability) => {
                let present = credentials.capability_bounding & (1_u64 << capability) != 0;
                return Some(LinuxResult::Value(u64::from(present)));
            }
            PrctlPlan::DropCapability(capability) => {
                if !credentials.has_capability(1_u64 << 8) {
                    return Some(LinuxResult::Error(Errno::EPERM));
                }
                credentials.capability_bounding &= !(1_u64 << capability);
            }
            PrctlPlan::GetSecureBits => {
                return Some(LinuxResult::Value(u64::from(credentials.secure_bits)));
            }
            PrctlPlan::SetSecureBits(value) => {
                if !credentials.has_capability(1_u64 << 8) || !Self::secure_change(credentials.secure_bits, value) {
                    return Some(LinuxResult::Error(Errno::EPERM));
                }
                credentials.secure_bits = value;
            }
            PrctlPlan::AmbientRead(capability) => {
                let present = credentials.capabilities.ambient & (1_u64 << capability) != 0;
                return Some(LinuxResult::Value(u64::from(present)));
            }
            PrctlPlan::AmbientRaise(capability) => {
                let bit = 1_u64 << capability;
                let permitted = credentials.capabilities.permitted & bit != 0
                    && credentials.capabilities.inheritable & bit != 0
                    && credentials.secure_bits & 64 == 0;
                if !permitted {
                    return Some(LinuxResult::Error(Errno::EPERM));
                }
                credentials.capabilities.ambient |= bit;
            }
            PrctlPlan::AmbientLower(capability) => {
                credentials.capabilities.ambient &= !(1_u64 << capability);
            }
            PrctlPlan::AmbientClear => credentials.capabilities.ambient = 0,
            _ => return None,
        }
        Some(self.replace_credentials(credentials))
    }

    fn secure_change(current: u32, requested: u32) -> bool {
        [(1, 2), (4, 8), (16, 32), (64, 128)]
            .into_iter()
            .all(|(setting, locked)| {
                current & locked == 0 || requested & locked != 0 && current & setting == requested & setting
            })
    }
}
