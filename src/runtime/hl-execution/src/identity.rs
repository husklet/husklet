use hl_isa::{GuestArchitecture, HostArchitecture};

use crate::DIGEST_SEED;

const PRIME: u64 = 1_099_511_628_211;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub regular: bool,
    pub stable_device: u64,
    pub stable_object: u64,
    pub size: u64,
    pub modified_ns: u64,
}

pub struct CacheIdentity;

impl CacheIdentity {
    pub fn name(name: Option<&str>) -> u64 {
        let Some(name) = name else { return 0x1357 };
        let mut value = DIGEST_SEED;
        for byte in name.rsplit('/').next().unwrap_or_default().bytes() {
            value = (value ^ u64::from(byte)).wrapping_mul(PRIME);
        }
        value
    }

    pub fn file(file: FileIdentity) -> u64 {
        if !file.regular {
            return 0;
        }
        Self::words(&[
            file.stable_device,
            file.stable_object,
            file.size,
            file.modified_ns / 1_000_000_000,
            file.modified_ns % 1_000_000_000,
        ])
    }

    pub const fn mix(program: u64, interpreter: u64, engine: u64, name: u64) -> u64 {
        (program ^ interpreter.wrapping_mul(PRIME)) ^ engine ^ name.wrapping_mul(PRIME)
    }

    pub fn configuration(build: u64, guest: GuestArchitecture, host: HostArchitecture, modes: u64) -> u64 {
        Self::words(&[build, guest as u64, host as u64, modes])
    }

    fn words(words: &[u64]) -> u64 {
        let mut value = DIGEST_SEED;
        for word in words {
            value = (value ^ word).wrapping_mul(PRIME);
        }
        value
    }
}
