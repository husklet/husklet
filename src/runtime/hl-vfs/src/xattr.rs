use std::collections::BTreeMap;
use std::sync::{Mutex, RwLock};

use crate::Identity;

pub const XATTR_NAME_MAXIMUM: usize = 255;
pub const XATTR_VALUE_MAXIMUM: usize = 65_536;
pub const XATTR_LIST_MAXIMUM: usize = 65_536;

/// One Linux extended-attribute name, preserving its exact guest bytes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct XattrName(Vec<u8>);

impl XattrName {
    pub fn new(bytes: &[u8]) -> Result<Self, XattrError> {
        if bytes.is_empty() || bytes.len() > XATTR_NAME_MAXIMUM || bytes.contains(&0) {
            return Err(XattrError::InvalidName);
        }
        Ok(Self(bytes.to_vec()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Linux `XATTR_CREATE/XATTR_REPLACE` selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum XattrFlags {
    #[default]
    Upsert,
    Create,
    Replace,
}

impl XattrFlags {
    pub fn from_bits(bits: u32) -> Result<Self, XattrError> {
        match bits {
            0 => Ok(Self::Upsert),
            1 => Ok(Self::Create),
            2 => Ok(Self::Replace),
            _ => Err(XattrError::InvalidFlags),
        }
    }
}

/// One staged extended-attribute mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XattrMutation<'value> {
    Set {
        name: &'value XattrName,
        value: &'value [u8],
    },
    Remove {
        name: &'value XattrName,
    },
}

/// Narrow transactional persistence port for extended attributes.
pub trait XattrHost: Send + Sync {
    type Transaction: Copy + Send;

    fn begin_xattr(&self, file: Identity) -> Result<Self::Transaction, XattrError>;

    fn stage_xattr(&self, transaction: Self::Transaction, mutation: XattrMutation<'_>) -> Result<(), XattrError>;

    fn commit_xattr(&self, transaction: Self::Transaction) -> Result<(), XattrError>;

    fn rollback_xattr(&self, transaction: Self::Transaction);
}

/// Bounded guest-visible xattr state with failure-atomic host publication.
pub struct Xattrs<H: XattrHost> {
    host: H,
    file: Identity,
    values: RwLock<BTreeMap<XattrName, Vec<u8>>>,
    mutation: Mutex<()>,
}

impl<H: XattrHost> Xattrs<H> {
    pub fn new(host: H, file: Identity) -> Self {
        Self {
            host,
            file,
            values: RwLock::new(BTreeMap::new()),
            mutation: Mutex::new(()),
        }
    }

    /// Returns the required value size and copies only when `output` fits.
    pub fn get(&self, name: &XattrName, output: Option<&mut [u8]>) -> Result<usize, XattrError> {
        let values = self.values.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = values.get(name).ok_or(XattrError::NoData)?;
        let Some(output) = output else {
            return Ok(value.len());
        };
        if output.is_empty() {
            return Ok(value.len());
        }
        if output.len() < value.len() {
            return Err(XattrError::Range);
        }
        output[..value.len()].copy_from_slice(value);
        Ok(value.len())
    }

    /// Returns or copies the NUL-separated guest-visible name list.
    pub fn list(&self, output: Option<&mut [u8]>) -> Result<usize, XattrError> {
        let values = self.values.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let required = values.keys().map(|name| name.as_bytes().len() + 1).sum::<usize>();
        if required > XATTR_LIST_MAXIMUM {
            return Err(XattrError::ListTooLarge);
        }
        let Some(output) = output else {
            return Ok(required);
        };
        if output.is_empty() {
            return Ok(required);
        }
        if output.len() < required {
            return Err(XattrError::Range);
        }
        let mut cursor = 0;
        for name in values.keys() {
            let end = cursor + name.as_bytes().len();
            output[cursor..end].copy_from_slice(name.as_bytes());
            output[end] = 0;
            cursor = end + 1;
        }
        Ok(required)
    }

    pub fn set(&self, name: &XattrName, value: &[u8], flags: XattrFlags) -> Result<(), XattrError> {
        if value.len() > XATTR_VALUE_MAXIMUM {
            return Err(XattrError::ValueTooLarge);
        }
        let _serial = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let exists = self
            .values
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(name);
        match (flags, exists) {
            (XattrFlags::Create, true) => return Err(XattrError::AlreadyExists),
            (XattrFlags::Replace, false) => return Err(XattrError::NoData),
            _ => {}
        }
        self.publish(XattrMutation::Set { name, value }, Some((name.clone(), value.to_vec())))
    }

    pub fn remove(&self, name: &XattrName) -> Result<(), XattrError> {
        let _serial = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let exists = self
            .values
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(name);
        if !exists {
            return Err(XattrError::NoData);
        }
        self.publish(XattrMutation::Remove { name }, None)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn publish(
        &self,
        mutation: XattrMutation<'_>,
        replacement: Option<(XattrName, Vec<u8>)>,
    ) -> Result<(), XattrError> {
        let mut transaction = XattrTransaction::begin(&self.host, self.file)?;
        transaction.stage(mutation.clone())?;
        transaction.commit()?;
        let mut values = self.values.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((name, value)) = replacement {
            values.insert(name, value);
        } else {
            let XattrMutation::Remove { name } = mutation else {
                return Err(XattrError::Host);
            };
            values.remove(name);
        }
        Ok(())
    }
}

struct XattrTransaction<'host, H: XattrHost> {
    host: &'host H,
    transaction: H::Transaction,
    active: bool,
}

impl<'host, H: XattrHost> XattrTransaction<'host, H> {
    fn begin(host: &'host H, file: Identity) -> Result<Self, XattrError> {
        Ok(Self {
            host,
            transaction: host.begin_xattr(file)?,
            active: true,
        })
    }

    fn stage(&mut self, mutation: XattrMutation<'_>) -> Result<(), XattrError> {
        self.host.stage_xattr(self.transaction, mutation)
    }

    fn commit(mut self) -> Result<(), XattrError> {
        self.host.commit_xattr(self.transaction)?;
        self.active = false;
        Ok(())
    }
}

impl<H: XattrHost> Drop for XattrTransaction<'_, H> {
    fn drop(&mut self) {
        if self.active {
            self.host.rollback_xattr(self.transaction);
        }
    }
}

/// Stable xattr errors mapped to Linux errno at the personality boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XattrError {
    InvalidFlags,
    InvalidName,
    ValueTooLarge,
    ListTooLarge,
    AlreadyExists,
    NoData,
    Range,
    ReadOnly,
    Host,
}
