use hl_isa::GuestArchitecture;

pub const UTS_FIELD_SIZE: usize = 65;
pub const UTS_SIZE: usize = UTS_FIELD_SIZE * 6;

/// Stable Linux identity encoded in the guest `struct utsname` layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UtsName {
    bytes: [u8; UTS_SIZE],
}

impl UtsName {
    #[must_use]
    pub fn engine(architecture: GuestArchitecture) -> Self {
        Self::identity(architecture, b"jit", b"")
    }

    #[must_use]
    pub fn identity(architecture: GuestArchitecture, hostname: &[u8], domainname: &[u8]) -> Self {
        let machine = match architecture {
            GuestArchitecture::Aarch64 => b"aarch64".as_slice(),
            GuestArchitecture::X86_64 => b"x86_64".as_slice(),
        };
        let mut bytes = [0; UTS_SIZE];
        Self::field(&mut bytes, 0, b"Linux");
        Self::field(&mut bytes, 1, hostname);
        Self::field(&mut bytes, 2, b"6.1.0");
        Self::field(&mut bytes, 3, b"#1 jit");
        Self::field(&mut bytes, 4, machine);
        Self::field(&mut bytes, 5, domainname);
        Self { bytes }
    }

    #[must_use]
    pub const fn bytes(&self) -> &[u8; UTS_SIZE] {
        &self.bytes
    }

    fn field(output: &mut [u8; UTS_SIZE], index: usize, value: &[u8]) {
        let start = index * UTS_FIELD_SIZE;
        let length = value.len().min(UTS_FIELD_SIZE - 1);
        output[start..start + length].copy_from_slice(&value[..length]);
    }
}
