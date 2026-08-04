use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_execution::{Aarch64CpuState, CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_isa::GuestArchitecture;
use hl_loader::{
    AddressSpaceError, ImageProtectionRegistry, InitialTlsPlan, LoadedProcess, MappingKind, MappingPlacement,
    Protection, ReservedMapping, ThreadLocalStorage, TransactionalAddressSpace,
};
use hl_runtime::{
    ExecLoadContext, ExecutionImageBuilder, PreparedDescriptorExec, PreparedExec, PreparedExecParticipant,
    PreparedLoaderExec, RuntimeExecError, RuntimeExecParticipant, RuntimeExecPort, SpaceFactory,
};
use hl_task::{ProcessId, ThreadId};

use super::super::{AddressSpaceAdapter, Reservation};
use super::source::Sources;
use super::space::AddressSpace;

pub(super) struct Registration {
    slot: Arc<hl_runtime::ExecSlot>,
    process: ProcessId,
}

impl Registration {
    pub(super) fn new(slot: Arc<hl_runtime::ExecSlot>, process: ProcessId) -> Arc<Self> {
        Arc::new(Self { slot, process })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.slot.unregister(self.process);
    }
}

/// Loader mutation capability paired with the exact application address space
/// that will later back instruction fetch, guest copy, futexes, and signal
/// frames. Keeping both in one value prevents exec composition from rebuilding
/// a look-alike adapter around a different mapping coordinator.
pub(super) struct ImageSpace {
    loader: AddressSpaceAdapter,
    space: Arc<AddressSpace>,
}

impl ImageSpace {
    pub(super) fn new(space: Arc<AddressSpace>) -> Self {
        let loader = AddressSpaceAdapter::from_memory(space.mappings(), space.arena().length());
        Self { loader, space }
    }

    pub(super) fn space(&self) -> Arc<AddressSpace> {
        Arc::clone(&self.space)
    }
}

impl TransactionalAddressSpace for ImageSpace {
    type Reservation = Reservation;

    fn reserve(
        &mut self,
        kind: MappingKind,
        size: u64,
        placement: MappingPlacement,
    ) -> Result<ReservedMapping<Self::Reservation>, AddressSpaceError> {
        self.loader.reserve(kind, size, placement)
    }

    fn stage_write(&mut self, token: &Self::Reservation, offset: u64, bytes: &[u8]) -> Result<(), AddressSpaceError> {
        self.loader.stage_write(token, offset, bytes)
    }

    fn stage_zero(&mut self, token: &Self::Reservation, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        self.loader.stage_zero(token, offset, size)
    }

    fn stage_protection(
        &mut self,
        token: &Self::Reservation,
        offset: u64,
        size: u64,
        protection: Protection,
    ) -> Result<(), AddressSpaceError> {
        self.loader.stage_protection(token, offset, size, protection)
    }

    fn commit(&mut self, tokens: &[Self::Reservation]) -> Result<(), AddressSpaceError> {
        self.loader.commit(tokens)
    }

    fn rollback(&mut self, token: &Self::Reservation) {
        self.loader.rollback(token);
    }
}

impl ImageProtectionRegistry<Reservation> for ImageSpace {
    fn stage_executable(&mut self, token: &Reservation, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        self.loader.stage_executable(token, offset, size)
    }

    fn stage_guest_access(
        &mut self,
        token: &Reservation,
        address: u64,
        size: u64,
        read_only: bool,
    ) -> Result<(), AddressSpaceError> {
        self.loader.stage_guest_access(token, address, size, read_only)
    }
}

pub(super) struct Spaces {
    current: Arc<AddressSpace>,
    next: AtomicU64,
}

impl Spaces {
    pub(super) fn new(current: Arc<AddressSpace>) -> Self {
        Self {
            current,
            next: AtomicU64::new(2),
        }
    }
}

impl SpaceFactory for Spaces {
    type AddressSpace = ImageSpace;

    fn create(&self, _: ProcessId) -> Result<Self::AddressSpace, RuntimeExecError> {
        let slot = self.next.fetch_add(1, Ordering::Relaxed);
        let space = self
            .current
            .exec_space(hl_memory::AddressSpaceId { slot, generation: 1 })
            .map_err(|_| RuntimeExecError::NoMemory)?;
        Ok(ImageSpace::new(space))
    }
}

pub(super) struct LoadContext {
    tasks: Arc<hl_task::TaskRegistry>,
    features: hl_loader::GuestFeatures,
    entropy: Arc<dyn super::ports::random::EntropySource>,
}

impl LoadContext {
    pub(super) fn new(
        tasks: Arc<hl_task::TaskRegistry>,
        features: hl_loader::GuestFeatures,
        entropy: Arc<dyn super::ports::random::EntropySource>,
    ) -> Self {
        Self {
            tasks,
            features,
            entropy,
        }
    }
}

impl ExecLoadContext for LoadContext {
    fn random(&self) -> Result<[u8; 16], RuntimeExecError> {
        super::image_data::Entropy::read_from(self.entropy.as_ref())
    }

    fn credentials(&self, process: ProcessId) -> Result<hl_loader::GuestCredentials, RuntimeExecError> {
        let snapshot = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|candidate| candidate.id == process)
            .ok_or(RuntimeExecError::Failed)?;
        Ok(hl_loader::GuestCredentials {
            user: snapshot.credentials.real_user,
            effective_user: snapshot.credentials.effective_user,
            group: snapshot.credentials.real_group,
            effective_group: snapshot.credentials.effective_group,
        })
    }

    fn features(&self) -> hl_loader::GuestFeatures {
        self.features
    }
}

pub(super) struct Tls;

impl ThreadLocalStorage for Tls {
    type Prepared = InitialTlsPlan;
    type Error = ();

    fn prepare_initial(&mut self, plan: &InitialTlsPlan) -> Result<Self::Prepared, Self::Error> {
        Ok(plan.clone())
    }
}

pub(super) struct CpuImage;

impl ExecutionImageBuilder<InitialTlsPlan> for CpuImage {
    type Image = ExecutionSnapshot;

    fn build(
        &self,
        architecture: GuestArchitecture,
        loaded: &LoadedProcess,
        _: &InitialTlsPlan,
    ) -> Result<Self::Image, RuntimeExecError> {
        let entry = loaded.dynamic_handoff().start_entry();
        let stack = loaded.initial_stack().stack_pointer();
        let cpu = match architecture {
            GuestArchitecture::Aarch64 => {
                let mut cpu = Aarch64CpuState::default();
                cpu.pc = entry;
                cpu.sp = stack;
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            GuestArchitecture::X86_64 => {
                let mut cpu = CpuState::default();
                cpu.rip = entry;
                cpu.registers[4] = stack;
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        };
        Ok(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        })
    }
}

pub(super) type LoaderExec = hl_runtime::LoaderExecParticipant<Sources, Spaces, Tls, CpuImage>;

pub(super) struct Coordinator {
    process: Arc<Mutex<std::sync::Weak<super::routing::ProcessContext>>>,
    threads: Arc<super::threads::ThreadSet>,
    loader: Arc<LoaderExec>,
    tasks: hl_runtime::TaskExecParticipant,
    ipc: hl_runtime::EmptyIpcExec,
    active: Arc<Mutex<bool>>,
    root: Option<Vec<u8>>,
    architecture: GuestArchitecture,
    limits: hl_loader::LoadLimits,
    context: Arc<LoadContext>,
    authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
}

impl Coordinator {
    pub(super) fn new(
        process: Arc<super::routing::ProcessContext>,
        threads: Arc<super::threads::ThreadSet>,
        loader: Arc<LoaderExec>,
        tasks: hl_runtime::TaskExecParticipant,
        root: Option<Vec<u8>>,
        architecture: GuestArchitecture,
        limits: hl_loader::LoadLimits,
        context: Arc<LoadContext>,
        authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
    ) -> Self {
        Self {
            process: Arc::new(Mutex::new(Arc::downgrade(&process))),
            threads,
            loader,
            tasks,
            ipc: hl_runtime::EmptyIpcExec,
            active: Arc::new(Mutex::new(false)),
            root,
            architecture,
            limits,
            context,
            authority,
        }
    }

    pub(super) fn fork(&self, process: Arc<super::routing::ProcessContext>) -> Arc<Self> {
        let (_, current) = self.loader.current();
        let space = process.space();
        let loader = Arc::new(LoaderExec::new(
            self.architecture,
            self.limits,
            Sources::new(self.root.as_deref(), self.authority.clone()),
            Spaces::new(Arc::clone(&space)),
            self.context.clone(),
            Tls,
            CpuImage,
            hl_runtime::LoaderExecImage {
                address_space: ImageSpace::new(space),
                loaded: current.loaded.clone(),
                tls: current.tls.clone(),
                execution: current.execution.clone(),
            },
        ));
        Arc::new(Self::new(
            Arc::clone(&process),
            Arc::clone(&self.threads),
            loader,
            hl_runtime::TaskExecParticipant::new(process.tasks()),
            self.root.clone(),
            self.architecture,
            self.limits,
            self.context.clone(),
            self.authority.clone(),
        ))
    }

    fn stage(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &hl_linux::ExecPlan,
    ) -> Result<Transaction, RuntimeExecError> {
        let mut active = self.active.lock().map_err(|_| RuntimeExecError::Failed)?;
        if *active {
            return Err(RuntimeExecError::Failed);
        }
        *active = true;
        drop(active);
        match self.stage_inner(process, thread, plan) {
            Ok(prepared) => Ok(prepared),
            Err(error) => {
                *self.active.lock().unwrap_or_else(|lock| lock.into_inner()) = false;
                Err(error)
            }
        }
    }

    fn stage_inner(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &hl_linux::ExecPlan,
    ) -> Result<Transaction, RuntimeExecError> {
        let current = self
            .process
            .lock()
            .map_err(|_| RuntimeExecError::Failed)?
            .upgrade()
            .ok_or(RuntimeExecError::Failed)?;
        if current.process() != process {
            return Err(RuntimeExecError::Failed);
        }
        let target = current.stage_exec(thread, plan)?;
        let identity = target.identity;
        let executable = target.executable;
        let source_files = current.files(thread);
        let descriptors =
            hl_runtime::DescriptorExec::new(source_files.image_slot(), current.epoll()).prepare_current()?;
        let table = descriptors.candidate().ok_or(RuntimeExecError::Failed)?;
        let loader = self.loader.prepare_resolved(process, &target.plan, &target.execfn)?;
        let image = loader.candidate().ok_or(RuntimeExecError::Failed)?;
        image.address_space.space().publish_procfs_image(
            &image.loaded,
            target.execfn.clone(),
            target.plan.environment.clone(),
        );
        let auxiliary = super::image_data::AuxiliaryImage::encode(image.loaded.initial_stack());
        let auxiliary_slot = current.auxiliary_slot().map_err(|_| RuntimeExecError::Failed)?;
        let retire = current.prepare_exec_retire(thread)?;
        let (tasks, resulting) = self.tasks.prepare_target(process, thread, plan)?;
        let context = current
            .from_candidate(
                source_files,
                table,
                image.address_space.space(),
                executable.clone(),
                auxiliary.clone(),
            )
            .map_err(|_| RuntimeExecError::Failed)?;
        let cancellation = Arc::new(super::readiness::Cancellation::new().map_err(|_| RuntimeExecError::Failed)?);
        let router = context
            .bind_candidate(&self.threads, resulting, Arc::clone(&cancellation))
            .map_err(|_| RuntimeExecError::Failed)?;
        let threads = self
            .threads
            .prepare_image(
                thread,
                resulting,
                router,
                cancellation,
                image.address_space.space(),
                image.execution.clone(),
            )
            .map_err(|_| RuntimeExecError::Failed)?;
        let ipc = self.ipc.prepare(process, thread, plan)?;
        Ok(Transaction {
            descriptors,
            loader,
            threads,
            retire,
            tasks,
            ipc,
            active: Arc::clone(&self.active),
            complete: false,
            process: context,
            process_slot: Arc::clone(&self.process),
            identity,
            executable,
            auxiliary_slot,
            auxiliary,
            previous_auxiliary: None,
            vfork: current.vfork_token(),
        })
    }
}
impl RuntimeExecPort for Coordinator {
    fn validate(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &hl_linux::ExecPlan,
    ) -> Result<(), RuntimeExecError> {
        let current = self
            .process
            .lock()
            .map_err(|_| RuntimeExecError::Failed)?
            .upgrade()
            .ok_or(RuntimeExecError::Failed)?;
        if current.process() != process {
            return Err(RuntimeExecError::Failed);
        }
        current.stage_exec(thread, plan).map(drop)
    }
    fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: hl_linux::ExecPlan,
    ) -> Result<Box<dyn PreparedExec>, RuntimeExecError> {
        Ok(Box::new(self.stage(process, thread, &plan)?))
    }
}

struct Transaction {
    descriptors: PreparedDescriptorExec,
    loader: PreparedLoaderExec<ImageSpace, InitialTlsPlan, ExecutionSnapshot>,
    threads: super::threads::PreparedImage,
    retire: super::exec_retire::RetireImage,
    tasks: Box<dyn PreparedExecParticipant>,
    ipc: Box<dyn PreparedExecParticipant>,
    active: Arc<Mutex<bool>>,
    complete: bool,
    process: Arc<super::routing::ProcessContext>,
    process_slot: Arc<Mutex<std::sync::Weak<super::routing::ProcessContext>>>,
    identity: Arc<Mutex<Vec<u8>>>,
    executable: Vec<u8>,
    auxiliary_slot: Arc<Mutex<Vec<u8>>>,
    auxiliary: Vec<u8>,
    previous_auxiliary: Option<Vec<u8>>,
    vfork: Option<Arc<hl_runtime::VforkParentToken>>,
}

impl Transaction {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.ipc.publish()?;
        self.descriptors.publish()?;
        self.loader.publish()?;
        let mut auxiliary = self.auxiliary_slot.lock().map_err(|_| RuntimeExecError::Failed)?;
        self.previous_auxiliary = Some(std::mem::replace(&mut *auxiliary, self.auxiliary.clone()));
        drop(auxiliary);
        self.threads.publish().map_err(|_| RuntimeExecError::Failed)?;
        self.retire.publish()?;
        self.tasks.publish()?;
        self.process.publish_procfs();
        *self.process_slot.lock().map_err(|_| RuntimeExecError::Failed)? = Arc::downgrade(&self.process);
        *self.identity.lock().map_err(|_| RuntimeExecError::Failed)? = self.executable.clone();
        Ok(())
    }

    fn rollback(&mut self) {
        self.tasks.rollback();
        self.retire.rollback();
        self.threads.rollback();
        if let Some(previous) = self.previous_auxiliary.take() {
            *self.auxiliary_slot.lock().unwrap_or_else(|error| error.into_inner()) = previous;
        }
        self.loader.rollback();
        self.descriptors.rollback();
        self.ipc.rollback();
    }

    fn finish(&mut self) {
        self.retire.finish();
        self.ipc.finish();
        self.descriptors.finish();
        self.loader.finish();
        self.threads.finish();
        self.tasks.finish();
        self.previous_auxiliary = None;
    }

    fn release(&self) {
        *self.active.lock().unwrap_or_else(|error| error.into_inner()) = false;
    }
}

impl PreparedExec for Transaction {
    fn commit(mut self: Box<Self>) -> Result<(), RuntimeExecError> {
        if let Err(error) = self.publish() {
            self.rollback();
            self.complete = true;
            self.release();
            return Err(error);
        }
        if let Some(token) = &self.vfork {
            let _ = token.release(self.process.process());
        }
        self.finish();
        self.complete = true;
        self.release();
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.complete {
            self.rollback();
            self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_memory::{AddressSpaceId, MappingCoordinator, SharedLimits, SharedObjectStore};

    #[test]
    fn exact_space() {
        let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let arena = Arc::new(
            super::super::VirtualMemory::reserve(16_384)
                .unwrap()
                .with_shared_store(Arc::clone(&shared))
                .with_snapshot_backings(),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            super::super::MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            AddressSpaceId { slot: 1, generation: 1 },
        ));
        let space = AddressSpace::new(arena, mappings);
        let image = ImageSpace::new(Arc::clone(&space));
        assert!(Arc::ptr_eq(&image.space(), &space));
    }
}
