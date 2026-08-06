use std::sync::Arc;

use hl_runtime::RuntimeAssembly;

use crate::activation::GuestIsa;
use crate::composition::RuntimeServices;
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::runtime_machine::GuestExecutionPort;

use super::{GuestExecutor, RuntimeLaunchPlan, readiness, task, threads};

impl GuestExecutionPort for GuestExecutor {
    fn start(
        &self,
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        assembly: &RuntimeAssembly,
        services: &RuntimeServices,
    ) -> Result<(), EngineError> {
        let key = std::ptr::from_ref::<RuntimeAssembly>(assembly) as usize;
        let cancellation = Arc::new(readiness::Cancellation::new().map_err(|_| EngineError::LaunchFailed)?);
        let counter: Arc<dyn hl_execution::ArchitecturalCounter> = Arc::new(task::HostCounter);
        let threads = Arc::new(
            threads::ThreadSet::with_counter(4096, assembly.tasks(), counter).map_err(|_| EngineError::LaunchFailed)?,
        );
        {
            let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
            if state.exits.contains_key(&key) {
                return Ok(());
            }
            state.running.insert(key, Arc::clone(&threads));
        }
        let result = self.run(isa, plan, assembly, services, cancellation, threads);
        let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        state.running.remove(&key);
        match result {
            Ok(exit) => {
                state.exits.insert(key, exit);
                Ok(())
            }
            Err(error) => {
                // Removing the live execution and returning before recording
                // its failure leaves every later waiter seeing Busy forever.
                state.failures.insert(key, error);
                Err(error)
            }
        }
    }

    fn wait(&self, assembly: &RuntimeAssembly) -> Result<EngineExit, EngineError> {
        let state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        let key = std::ptr::from_ref::<RuntimeAssembly>(assembly) as usize;
        if let Some(exit) = state.exits.get(&key) {
            return Ok(*exit);
        }
        Err(state.failures.get(&key).copied().unwrap_or(EngineError::Busy))
    }

    fn stop(&self, assembly: &RuntimeAssembly, request: StopRequest) -> Result<(), EngineError> {
        let key = std::ptr::from_ref::<RuntimeAssembly>(assembly) as usize;
        let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        if let Some(threads) = state.running.get(&key) {
            if matches!(request, StopRequest::Force) {
                threads.cancel_all(request.signal());
            } else {
                threads
                    .deliver_process_signal(request.signal())
                    .map_err(|_| EngineError::StopFailed)?;
            }
        } else {
            state.exits.entry(key).or_insert_with(|| Self::signal(request.signal()));
        }
        Ok(())
    }
}
