const UNIX_ADDRESS_MAXIMUM: usize = 108;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Address {
    Unnamed,
    Pathname(Vec<u8>),
    Abstract(Vec<u8>),
}
pub type UnixAddress = Address;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressError {
    Invalid,
}
pub type UnixAddressError = AddressError;

impl UnixAddress {
    pub fn pathname(value: Vec<u8>) -> Result<Self, UnixAddressError> {
        Self::bounded(value).map(Self::Pathname)
    }

    pub fn abstract_name(value: Vec<u8>) -> Result<Self, UnixAddressError> {
        Self::bounded(value).map(Self::Abstract)
    }

    fn bounded(value: Vec<u8>) -> Result<Vec<u8>, UnixAddressError> {
        if value.is_empty() || value.len() > UNIX_ADDRESS_MAXIMUM {
            return Err(UnixAddressError::Invalid);
        }
        Ok(value)
    }
}
