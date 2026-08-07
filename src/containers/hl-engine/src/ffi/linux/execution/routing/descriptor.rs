//! Descriptor selection and publication.

use std::sync::{Arc, Weak};

use super::super::readiness;
use super::ProcessContext;

impl ProcessContext {
    pub(in crate::ffi::linux::execution) fn files(
        &self,
        thread: hl_task::ThreadId,
    ) -> Arc<hl_runtime::RuntimeDescriptorTable> {
        let mut files = self
            .thread_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        files.retain(|_, private| private.table.strong_count() != 0);
        files
            .get(&thread)
            .and_then(|private| Weak::upgrade(&private.table))
            .unwrap_or_else(|| Arc::clone(&self.epoll_table))
    }

    pub(in crate::ffi::linux::execution) fn inherit_files(
        &self,
        source: hl_task::ThreadId,
        child: hl_task::ThreadId,
    ) -> Arc<hl_runtime::RuntimeDescriptorTable> {
        let table = self.files(source);
        if !Arc::ptr_eq(&table, &self.epoll_table) {
            let permit = self
                .thread_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&source)
                .map(|private| Arc::clone(&private.permit))
                .expect("private descriptor table has admission");
            self.thread_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    child,
                    super::PrivateTable {
                        table: Arc::downgrade(&table),
                        permit,
                    },
                );
        }
        table
    }

    pub(in crate::ffi::linux::execution) fn forget_files(&self, thread: hl_task::ThreadId) {
        self.thread_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&thread);
    }

    pub(in crate::ffi::linux::execution) fn publish_unshare(
        self: &Arc<Self>,
        thread: hl_task::ThreadId,
        cancellation: Arc<readiness::Cancellation>,
        first: u32,
        last: u32,
        close_on_exec: bool,
    ) -> Result<(), hl_runtime::ControlError> {
        let permit = self
            .table_admission
            .reserve()
            .ok_or(hl_runtime::ControlError::Capacity)?;
        let current = self.files(thread);
        let candidate = Arc::new(self.epoll.unshare_range(&current, first, last, close_on_exec)?);
        let threads = self
            .threads
            .get()
            .and_then(Weak::upgrade)
            .ok_or(hl_runtime::ControlError::Descriptor(
                hl_descriptor::DescriptorError::Corrupt,
            ))?;
        let clone = self
            .clone_context
            .get()
            .and_then(Weak::upgrade)
            .and_then(|context| context.runtime())
            .map(|runtime| {
                Box::new(hl_runtime::ThreadCloneTrap::new(runtime, thread)) as Box<dyn hl_runtime::ThreadCloneTrapPort>
            });
        self.thread_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                thread,
                super::PrivateTable {
                    table: Arc::downgrade(&candidate),
                    permit,
                },
            );
        let router = Arc::new(self.router(thread, cancellation, clone));
        if threads.replace_router(thread, router).is_err() {
            self.forget_files(thread);
            return Err(hl_runtime::ControlError::Descriptor(
                hl_descriptor::DescriptorError::Corrupt,
            ));
        }
        Ok(())
    }
}
