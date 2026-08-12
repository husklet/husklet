#![allow(unsafe_code)]

use crate::activation::GuestIsa;
use crate::engine::EngineError;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;

#[derive(Debug, Eq, PartialEq)]
#[repr(C)]
pub(in crate::execution) struct CMainImagePlan {
    pub(in crate::execution) abi: u32,
    pub(in crate::execution) size: u32,
    pub(in crate::execution) architecture: u32,
    pub(in crate::execution) kind: u32,
    pub(in crate::execution) link_start: u64,
    pub(in crate::execution) link_end: u64,
    pub(in crate::execution) has_interpreter: u32,
    pub(in crate::execution) reserved: u32,
    pub(in crate::execution) interpreter_identity: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub(in crate::execution) struct CAddressProjection {
    pub(in crate::execution) abi: u32,
    pub(in crate::execution) size: u32,
    pub(in crate::execution) flags: u32,
    pub(in crate::execution) reserved: u32,
    pub(in crate::execution) guest_start: u64,
    pub(in crate::execution) guest_end: u64,
    pub(in crate::execution) storage_bias: u64,
}

struct CImageFile(std::fs::File);

impl hl_loader::ImageReadAt for CImageFile {
    fn length(&self) -> Result<u64, ()> {
        self.0.metadata().map(|metadata| metadata.len()).map_err(|_| ())
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), ()> {
        self.0.read_exact_at(output, offset).map_err(|_| ())
    }
}

pub(in crate::execution) fn c_main_image_plan(
    isa: GuestIsa,
    path: Option<&CString>,
    authority: Option<&crate::executable::ExecutableAuthority>,
) -> Result<CMainImagePlan, EngineError> {
    let source = if authority.is_some() { "authority" } else { "path" };
    let reject = |stage| {
        hl_log::hl_verdict!(
            hl_log::tag::EXEC,
            "execution.c.image_plan.rejected",
            isa = ?isa,
            source = %source,
            stage = %stage;
            "retained C image plan rejected isa={isa:?} source={source} stage={stage}"
        );
        EngineError::LaunchFailed
    };
    let file = if let Some(authority) = authority {
        // SAFETY: dup creates independent ownership; File closes only that duplicate.
        let descriptor = unsafe { libc::dup(authority.descriptor().as_raw_fd()) };
        if descriptor < 0 {
            return Err(reject("duplicate"));
        }
        // SAFETY: descriptor is the newly owned duplicate above.
        unsafe { std::fs::File::from_raw_fd(descriptor) }
    } else {
        let path = path.ok_or_else(|| reject("select"))?;
        std::fs::File::open(std::ffi::OsStr::from_bytes(path.as_bytes())).map_err(|_| reject("open"))?
    };
    let architecture = match isa {
        GuestIsa::Aarch64 => hl_isa::GuestArchitecture::Aarch64,
        GuestIsa::X86_64 => hl_isa::GuestArchitecture::X86_64,
    };
    let plan = hl_loader::MainImageInspector::new(architecture, hl_loader::ImageLimits::default())
        .inspect(&CImageFile(file))
        .map_err(|_| reject("inspect"))?;
    let kind = match plan.kind {
        hl_loader::ImageKind::Executable => 1,
        hl_loader::ImageKind::PositionIndependent => 2,
    };
    let interpreter_identity = plan.interpreter.as_deref().map_or(0, |path| {
        path.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    });
    Ok(CMainImagePlan {
        abi: 1,
        size: u32::try_from(std::mem::size_of::<CMainImagePlan>()).unwrap(),
        architecture: isa as u32,
        kind,
        link_start: plan.link_start,
        link_end: plan.link_end,
        has_interpreter: u32::from(plan.interpreter.is_some()),
        reserved: 0,
        interpreter_identity,
    })
}

#[cfg(test)]
unsafe extern "C" {
    pub(in crate::execution) fn hl_native_address_projection_init(
        projection: *mut CAddressProjection,
        guest_start: u64,
        guest_end: u64,
        storage_start: u64,
    ) -> libc::c_int;
    pub(in crate::execution) fn hl_native_address_projection_init_elf(
        projection: *mut CAddressProjection,
        kind: u32,
        link_start: u64,
        link_end: u64,
        storage_start: u64,
    ) -> libc::c_int;
    pub(in crate::execution) fn hl_native_address_projection_storage(
        projection: *const CAddressProjection,
        guest: u64,
        storage: *mut u64,
    ) -> libc::c_int;
    pub(in crate::execution) fn hl_native_address_projection_guest(
        projection: *const CAddressProjection,
        storage: u64,
        guest: *mut u64,
    ) -> libc::c_int;
}
