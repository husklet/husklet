use std::sync::{Arc, Mutex};

use hl_isa::GuestArchitecture;
use hl_linux::ExecPlan;
use hl_loader::{
    GuestCredentials, GuestFeatures, ImageProtectionRegistry, ImageSource, LoadError, LoadLimits, LoadRequest,
    LoadedProcess, Loader, LoaderDiagnostic, ThreadLocalStorage, TransactionalAddressSpace,
};
use hl_task::{ProcessId, ThreadId};

use crate::{PreparedExecParticipant, ProcessImage, RuntimeExecError, RuntimeExecParticipant};

/// Receives the precise loader failure before it is projected to Linux's exec errno.
pub trait LoadFailureReporter: Send + Sync {
    fn report(&self, process: ProcessId, error: LoadError);
}

struct LogLoadFailure;

impl LoadFailureReporter for LogLoadFailure {
    fn report(&self, process: ProcessId, error: LoadError) {
        hl_log::hl_error!(
            hl_log::tag::EXEC,
            "exec image load failed: process={process:?} error={error}"
        );
    }
}

pub trait SourceFactory: Send + Sync {
    type Source: ImageSource;

    fn open(&self, process: ProcessId, plan: &ExecPlan) -> Result<Self::Source, RuntimeExecError>;
}

pub trait SpaceFactory: Send + Sync {
    type AddressSpace: TransactionalAddressSpace
        + ImageProtectionRegistry<<Self::AddressSpace as TransactionalAddressSpace>::Reservation>;

    fn create(&self, process: ProcessId) -> Result<Self::AddressSpace, RuntimeExecError>;
}

pub trait ExecLoadContext: Send + Sync {
    fn random(&self) -> Result<[u8; 16], RuntimeExecError>;
    fn credentials(&self, process: ProcessId) -> Result<GuestCredentials, RuntimeExecError>;
    fn features(&self) -> GuestFeatures;
}

pub trait ExecutionImageBuilder<T>: Send + Sync {
    type Image: Send + Sync + 'static;

    fn build(
        &self,
        architecture: GuestArchitecture,
        loaded: &LoadedProcess,
        tls: &T,
    ) -> Result<Self::Image, RuntimeExecError>;
}

pub struct Image<A, T, E> {
    pub address_space: A,
    pub loaded: LoadedProcess,
    pub tls: T,
    pub execution: E,
}

pub struct Participant<S, A, T, E>
where
    S: SourceFactory,
    A: SpaceFactory,
    T: ThreadLocalStorage + Send,
    E: ExecutionImageBuilder<T::Prepared>,
{
    architecture: GuestArchitecture,
    limits: LoadLimits,
    sources: S,
    address_spaces: A,
    context: Arc<dyn ExecLoadContext>,
    tls: Mutex<T>,
    execution: E,
    failures: Arc<dyn LoadFailureReporter>,
    diagnostics: Option<hl_log::Channel<LoaderDiagnostic>>,
    images: ProcessImage<Image<A::AddressSpace, T::Prepared, E::Image>>,
}

pub struct PreparedLoaderExec<A, T, E> {
    image: crate::PreparedProcessImage<Image<A, T, E>>,
}

impl<A, T, E> PreparedLoaderExec<A, T, E> {
    #[must_use]
    pub fn candidate(&self) -> Option<Arc<Image<A, T, E>>> {
        self.image.candidate()
    }
}

impl<A, T, E> PreparedExecParticipant for PreparedLoaderExec<A, T, E>
where
    A: Send + Sync + 'static,
    T: Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.image.publish()
    }

    fn rollback(&mut self) {
        self.image.rollback();
    }

    fn finish(&mut self) {
        self.image.finish();
    }
}

impl<S, A, T, E> Participant<S, A, T, E>
where
    S: SourceFactory,
    A: SpaceFactory,
    T: ThreadLocalStorage + Send,
    T::Prepared: Send + Sync + 'static,
    E: ExecutionImageBuilder<T::Prepared>,
    A::AddressSpace: Send + Sync + 'static,
{
    pub fn new(
        architecture: GuestArchitecture,
        limits: LoadLimits,
        sources: S,
        address_spaces: A,
        context: Arc<dyn ExecLoadContext>,
        tls: T,
        execution: E,
        initial: Image<A::AddressSpace, T::Prepared, E::Image>,
    ) -> Self {
        Self {
            architecture,
            limits,
            sources,
            address_spaces,
            context,
            tls: Mutex::new(tls),
            execution,
            failures: Arc::new(LogLoadFailure),
            diagnostics: None,
            images: ProcessImage::new(initial),
        }
    }

    /// Replaces the default release-safe log reporter, primarily for embedding and tests.
    pub fn with_failure_reporter(mut self, failures: Arc<dyn LoadFailureReporter>) -> Self {
        self.failures = failures;
        self
    }

    #[must_use]
    pub fn with_loader_diagnostics(mut self, diagnostics: hl_log::Channel<LoaderDiagnostic>) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    #[must_use]
    pub fn current(&self) -> (u64, Arc<Image<A::AddressSpace, T::Prepared, E::Image>>) {
        self.images.current()
    }

    pub fn prepare_current(
        &self,
        process: ProcessId,
        plan: &ExecPlan,
    ) -> Result<PreparedLoaderExec<A::AddressSpace, T::Prepared, E::Image>, RuntimeExecError> {
        self.prepare_resolved(process, plan, &plan.path)
    }

    pub fn prepare_resolved(
        &self,
        process: ProcessId,
        plan: &ExecPlan,
        execfn: &[u8],
    ) -> Result<PreparedLoaderExec<A::AddressSpace, T::Prepared, E::Image>, RuntimeExecError> {
        let (generation, _) = self.images.current();
        let candidate = self.candidate(process, plan, execfn)?;
        Ok(PreparedLoaderExec {
            image: self.images.prepare(generation, candidate),
        })
    }

    fn candidate(
        &self,
        process: ProcessId,
        plan: &ExecPlan,
        execfn: &[u8],
    ) -> Result<Image<A::AddressSpace, T::Prepared, E::Image>, RuntimeExecError> {
        let source = self.sources.open(process, plan)?;
        let address_space = self.address_spaces.create(process)?;
        let mut loader = Loader::new(source, address_space, self.limits);
        if let Some(diagnostics) = &self.diagnostics {
            loader = loader.with_diagnostics(diagnostics.clone());
        }
        let arguments = plan.arguments.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let environment = plan.environment.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let loaded = loader
            .load(LoadRequest {
                architecture: self.architecture,
                image_path: &plan.path,
                executable_path: execfn,
                arguments: &arguments,
                environment: &environment,
                random: self.context.random()?,
                credentials: self.context.credentials(process)?,
                features: self.context.features(),
            })
            .map_err(|error| {
                self.failures.report(process, error);
                Self::project_load(error)
            })?;
        let (_, address_space) = loader.into_parts();
        let tls = self
            .tls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .prepare_initial(loaded.initial_tls())
            .map_err(|_| RuntimeExecError::NoMemory)?;
        let execution = self.execution.build(self.architecture, &loaded, &tls)?;
        Ok(Image {
            address_space,
            loaded,
            tls,
            execution,
        })
    }

    const fn project_load(error: LoadError) -> RuntimeExecError {
        match error {
            LoadError::Source { error, .. } => match error {
                hl_loader::ImageSourceError::NotFound => RuntimeExecError::NotFound,
                hl_loader::ImageSourceError::AccessDenied => RuntimeExecError::Access,
                hl_loader::ImageSourceError::TooLarge => RuntimeExecError::TooBig,
                hl_loader::ImageSourceError::Io => RuntimeExecError::Failed,
            },
            LoadError::Inspect { .. } | LoadError::InvalidInterpreter => RuntimeExecError::Format,
            LoadError::AddressSpace(_)
            | LoadError::InvalidReservation
            | LoadError::Tls(_)
            | LoadError::Protection(_) => RuntimeExecError::NoMemory,
            LoadError::Stack(_) => RuntimeExecError::TooBig,
        }
    }
}

impl<S, A, T, E> RuntimeExecParticipant for Participant<S, A, T, E>
where
    S: SourceFactory,
    A: SpaceFactory,
    T: ThreadLocalStorage + Send,
    T::Prepared: Send + Sync + 'static,
    E: ExecutionImageBuilder<T::Prepared>,
    A::AddressSpace: Send + Sync + 'static,
{
    fn prepare(
        &self,
        process: ProcessId,
        _: ThreadId,
        plan: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        self.prepare_current(process, plan)
            .map(|prepared| Box::new(prepared) as Box<dyn PreparedExecParticipant>)
    }
}
