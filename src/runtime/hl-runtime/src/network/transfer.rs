use std::sync::Arc;

use hl_descriptor::{
    DescriptionIdentity, DescriptionRef, DescriptorFlags, DescriptorTable, OpenFileDescription, PreparedInstallBatch,
    StatusFlags,
};

use crate::RuntimeNetworkError;

pub struct ImportedDescription {
    pub object: Arc<dyn OpenFileDescription>,
    pub status: StatusFlags,
}

pub trait TransferPublication: Send {
    fn bind(&mut self, identities: &[DescriptionIdentity]) -> Result<(), RuntimeNetworkError>;
    fn commit(self: Box<Self>);
    fn rollback(self: Box<Self>);
}

pub struct ImportedTransfer {
    descriptions: Vec<ImportedDescription>,
    publication: Box<dyn TransferPublication>,
}

impl ImportedTransfer {
    #[must_use]
    pub fn new(descriptions: Vec<ImportedDescription>, publication: Box<dyn TransferPublication>) -> Self {
        Self {
            descriptions,
            publication,
        }
    }

    pub fn prepare<'table>(
        self,
        descriptors: &'table DescriptorTable,
        close_on_exec: bool,
    ) -> Result<PreparedTransfer<'table>, RuntimeNetworkError> {
        let flags = DescriptorFlags::from_bits(if close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let objects = self
            .descriptions
            .into_iter()
            .map(|description| (description.object, description.status, flags))
            .collect();
        let install = descriptors
            .prepare_open_batch(0, objects)
            .map_err(|_| RuntimeNetworkError::Failed)?;
        Ok(PreparedTransfer {
            install: Some(install),
            publication: Some(self.publication),
            bound: false,
        })
    }

    #[must_use]
    pub fn merge(transfers: Vec<Self>) -> Self {
        let mut descriptions = Vec::new();
        let mut publications = Vec::with_capacity(transfers.len());
        for transfer in transfers {
            let count = transfer.descriptions.len();
            descriptions.extend(transfer.descriptions);
            publications.push((count, transfer.publication));
        }
        Self::new(descriptions, Box::new(MergedPublication { publications, bound: 0 }))
    }
}

struct MergedPublication {
    publications: Vec<(usize, Box<dyn TransferPublication>)>,
    bound: usize,
}

impl TransferPublication for MergedPublication {
    fn bind(&mut self, identities: &[DescriptionIdentity]) -> Result<(), RuntimeNetworkError> {
        let expected = self
            .publications
            .iter()
            .try_fold(0_usize, |total, (count, _)| total.checked_add(*count))
            .ok_or(RuntimeNetworkError::Invalid)?;
        if expected != identities.len() {
            return Err(RuntimeNetworkError::Invalid);
        }
        let mut offset = 0_usize;
        for (count, publication) in &mut self.publications {
            let end = offset.checked_add(*count).ok_or(RuntimeNetworkError::Invalid)?;
            publication.bind(identities.get(offset..end).ok_or(RuntimeNetworkError::Invalid)?)?;
            self.bound += 1;
            offset = end;
        }
        Ok(())
    }

    fn commit(self: Box<Self>) {
        for (_, publication) in self.publications {
            publication.commit();
        }
    }

    fn rollback(mut self: Box<Self>) {
        for (_, publication) in self.publications.drain(..self.bound) {
            publication.rollback();
        }
    }
}

pub trait DescriptorTransfer<A>: Send + Sync {
    fn export(&self, description: &DescriptionRef) -> Result<A, RuntimeNetworkError>;
    fn import(&self, attachments: Vec<A>) -> Result<ImportedTransfer, RuntimeNetworkError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferCommitError<E> {
    Runtime(RuntimeNetworkError),
    Copyout(E),
}

pub struct PreparedTransfer<'table> {
    install: Option<PreparedInstallBatch<'table>>,
    publication: Option<Box<dyn TransferPublication>>,
    bound: bool,
}

impl<E> From<RuntimeNetworkError> for TransferCommitError<E> {
    fn from(error: RuntimeNetworkError) -> Self {
        Self::Runtime(error)
    }
}

impl PreparedTransfer<'_> {
    pub fn publish_after<E>(
        mut self,
        copyout: impl FnOnce(&[i32]) -> Result<(), E>,
    ) -> Result<Vec<i32>, TransferCommitError<E>> {
        let install = self.install.as_ref().expect("prepared transfer retains install");
        let identities = install.description_identities();
        let numbers = install.numbers();
        self.bound = true;
        self.publication
            .as_mut()
            .expect("prepared transfer retains publication")
            .bind(&identities)?;
        copyout(&numbers).map_err(TransferCommitError::Copyout)?;
        let published = self
            .install
            .take()
            .expect("prepared transfer retains install")
            .publish_all();
        self.publication
            .take()
            .expect("prepared transfer retains publication")
            .commit();
        self.bound = false;
        Ok(published)
    }
}

impl Drop for PreparedTransfer<'_> {
    fn drop(&mut self) {
        if self.bound {
            if let Some(publication) = self.publication.take() {
                publication.rollback();
            }
        }
    }
}
