use super::*;
use crate::test_support::ProcessFixture;
use crate::{
    Control, DescriptorExec, DescriptorImageSlot, ExecParticipant, ExecRole, ExecRuntime, ExecRuntimeDependencies,
    MemoryPort, PreparedExecParticipant, RuntimeAssembly, RuntimeAssemblyConfig, RuntimeDomain, RuntimeExecPort,
    TaskExecParticipant,
};

struct PublishFailureParticipant {
    inner: Arc<dyn RuntimeExecParticipant>,
    fail: bool,
}

struct PublishFailureStage {
    inner: Box<dyn PreparedExecParticipant>,
    fail: bool,
}

impl RuntimeExecParticipant for PublishFailureParticipant {
    fn prepare(
        &self,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
        plan: &hl_linux::ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        Ok(Box::new(PublishFailureStage {
            inner: self.inner.prepare(process, thread, plan)?,
            fail: self.fail,
        }))
    }
}

impl PreparedExecParticipant for PublishFailureStage {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.inner.publish()?;
        if self.fail {
            return Err(RuntimeExecError::Failed);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.inner.rollback();
    }

    fn finish(&mut self) {
        self.inner.finish();
    }
}

fn gated(
    participant: Arc<dyn RuntimeExecParticipant>,
    role: ExecRole,
    failed: Option<ExecRole>,
) -> Arc<dyn RuntimeExecParticipant> {
    Arc::new(PublishFailureParticipant {
        inner: participant,
        fail: failed == Some(role),
    })
}

struct IntegrationFixture;

impl IntegrationFixture {
    fn run_success(architecture: GuestArchitecture, dynamic: bool) {
        let fixture = ProcessFixture::new();
        let loader = Arc::new(LoaderExecParticipant::new(
            architecture,
            Fixture::limits(),
            Sources {
                architecture,
                dynamic,
                malformed: false,
                malformed_interpreter: false,
                nested_interpreter: false,
            },
            Spaces(None),
            Arc::new(Context),
            Tls,
            Execution,
            Fixture::initial(architecture),
        ));
        let (epoll, table) = Control::new(16, 16).unwrap();
        let descriptors = Arc::new(DescriptorImageSlot::from_shared(table.descriptor_table().clone()));
        let dependencies = ExecRuntimeDependencies::builder()
            .participant(
                ExecRole::Task,
                Arc::new(TaskExecParticipant::new(fixture.tasks.clone())),
            )
            .unwrap()
            .participant(
                ExecRole::DescriptorEpoll,
                Arc::new(DescriptorExec::new(descriptors, Arc::new(epoll))),
            )
            .unwrap()
            .participant(
                ExecRole::Ipc,
                Arc::new(ExecParticipant::new(
                    fixture.ipc.catalog.clone(),
                    fixture.mappings.clone(),
                    Arc::new(|| 9),
                )),
            )
            .unwrap()
            .participant(ExecRole::Loader, loader.clone())
            .unwrap()
            .build()
            .unwrap();
        let runtime = Arc::new(ExecRuntime::new(dependencies).unwrap());
        let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
        assembly.install_exec(runtime).unwrap();
        assembly.require(RuntimeDomain::Execution).unwrap();
        assembly
            .exec()
            .unwrap()
            .exec(fixture.process, fixture.thread, Fixture::plan())
            .unwrap();
        assert_eq!(loader.current().1.execution.0, architecture);
        assert_eq!(loader.current().1.loaded.interpreter().is_some(), dynamic,);
    }
}

#[test]
fn real_static_dynamic() {
    for (architecture, dynamic) in [
        (GuestArchitecture::Aarch64, false),
        (GuestArchitecture::Aarch64, true),
        (GuestArchitecture::X86_64, false),
        (GuestArchitecture::X86_64, true),
    ] {
        IntegrationFixture::run_success(architecture, dynamic);
    }
}

#[test]
fn real_domains_exactly() {
    for failed in [
        ExecRole::Task,
        ExecRole::DescriptorEpoll,
        ExecRole::Ipc,
        ExecRole::Loader,
    ] {
        let fixture = ProcessFixture::new();
        let loader = Arc::new(LoaderExecParticipant::new(
            GuestArchitecture::Aarch64,
            Fixture::limits(),
            Sources {
                architecture: GuestArchitecture::Aarch64,
                dynamic: true,
                malformed: false,
                malformed_interpreter: false,
                nested_interpreter: false,
            },
            Spaces(None),
            Arc::new(Context),
            Tls,
            Execution,
            Fixture::initial(GuestArchitecture::Aarch64),
        ));
        let (epoll, table) = Control::new(16, 16).unwrap();
        let epoll = Arc::new(epoll);
        let descriptors = Arc::new(DescriptorImageSlot::from_shared(table.descriptor_table().clone()));
        let task_before = fixture.tasks.snapshot();
        let descriptor_before = descriptors.current();
        let epoll_before = epoll.graph_snapshot();
        let bindings_before = fixture.mappings.bindings().unwrap();
        let regions_before = fixture.mappings.coordinator.ledger().regions();
        let shared_before = fixture.ipc.shared.snapshot();
        let semaphores_before = fixture.ipc.semaphores.snapshot();
        let loader_before = loader.current();

        let dependencies = ExecRuntimeDependencies::builder()
            .participant(
                ExecRole::Task,
                gated(
                    Arc::new(TaskExecParticipant::new(fixture.tasks.clone())),
                    ExecRole::Task,
                    Some(failed),
                ),
            )
            .unwrap()
            .participant(
                ExecRole::DescriptorEpoll,
                gated(
                    Arc::new(DescriptorExec::new(descriptors.clone(), epoll.clone())),
                    ExecRole::DescriptorEpoll,
                    Some(failed),
                ),
            )
            .unwrap()
            .participant(
                ExecRole::Ipc,
                gated(
                    Arc::new(ExecParticipant::new(
                        fixture.ipc.catalog.clone(),
                        fixture.mappings.clone(),
                        Arc::new(|| 9),
                    )),
                    ExecRole::Ipc,
                    Some(failed),
                ),
            )
            .unwrap()
            .participant(ExecRole::Loader, gated(loader.clone(), ExecRole::Loader, Some(failed)))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            ExecRuntime::new(dependencies)
                .unwrap()
                .exec(fixture.process, fixture.thread, Fixture::plan(),),
            Err(RuntimeExecError::Failed),
        );

        let task_after = fixture.tasks.snapshot();
        assert_eq!(task_after.processes, task_before.processes);
        assert_eq!(task_after.threads, task_before.threads);
        assert_eq!(task_after.thread_generations, task_before.thread_generations,);
        let descriptor_after = descriptors.current();
        assert_eq!(descriptor_after.0, descriptor_before.0);
        assert!(Arc::ptr_eq(&descriptor_after.1, &descriptor_before.1,));
        assert_eq!(epoll.graph_snapshot(), epoll_before);
        assert_eq!(fixture.mappings.bindings().unwrap(), bindings_before,);
        assert_eq!(fixture.mappings.coordinator.ledger().regions(), regions_before,);
        assert_eq!(fixture.ipc.shared.snapshot(), shared_before);
        assert_eq!(fixture.ipc.semaphores.snapshot(), semaphores_before,);
        let loader_after = loader.current();
        assert_eq!(loader_after.0, loader_before.0);
        assert!(Arc::ptr_eq(&loader_after.1, &loader_before.1));
    }
}
