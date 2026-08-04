use crate::{
    service::{OverlayConfig, ProcessConfig, Running, Runtime},
    Error, Result,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex as StdMutex};

mod process;
mod spec;
use process::Process;
use spec::Spec;

#[derive(Default)]
pub(crate) struct Engine;

#[async_trait]
impl Runtime for Engine {
    fn validate_overlay(&self, overlay: &OverlayConfig) -> bool {
        overlay.lower.is_dir() && overlay.upper.is_dir() && overlay.work.is_dir()
    }

    async fn start(&self, mut config: ProcessConfig) -> Result<Arc<dyn Running>> {
        if !config.rootfs.is_dir() {
            return Err(Error::InvalidSpec(format!(
                "rootfs does not exist or is not a directory: {}",
                config.rootfs.display()
            )));
        }
        if config.checkpoint.as_ref().is_some_and(|checkpoint| checkpoint.restore) {
            return Err(Error::Runtime(
                "Rust engine checkpoint restore is not connected to container storage".into(),
            ));
        }
        let checkpointable = false;
        let spec = Spec::try_from(&config)?;
        let engine = Arc::new(
            hl_engine::runtime::Engine::from_plan(spec.isa, spec.plan)
                .map_err(|error| Error::Runtime(format!("engine construction: {error:?}")))?,
        );
        engine
            .start()
            .map_err(|error| Error::Runtime(format!("engine start: {error:?}")))?;

        // The in-process Rust runtime stream ports are connected in the next
        // integration slice. Keep the channel contract explicit and closed so
        // consumers never wait forever for output that cannot arrive.
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(sender);
        drop(config.input.take());
        Ok(Arc::new(Process {
            id: Process::next_id(),
            child: StdMutex::new(Some(engine)),
            logs: StdMutex::new(Some(receiver)),
            domain: spec.domain,
            checkpointable,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::Spec;
    use crate::service::ProcessConfig;

    fn launch() -> ProcessConfig {
        ProcessConfig {
            network_namespace: "container-test".to_owned(),
            rootfs: "/rootfs".into(),
            overlay: None,
            owners: Vec::new(),
            filesystem_generation: "/generation".into(),
            translation_cache: None,
            checkpoint: None,
            guest: crate::Guest::Aarch64,
            process: crate::Process::new("/bin/true"),
            hostname: None,
            mounts: Vec::new(),
            resources: crate::Resources::default(),
            isolation: crate::Isolation::default(),
            network_mode: crate::NetworkMode::Automatic,
            networks: Vec::new(),
            publish: Vec::new(),
            input: None,
            terminal: None,
            domain: None,
            domain_owner: true,
        }
    }

    #[test]
    fn resolved_container_plan_uses_the_rust_engine() {
        let launch = launch();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.rootfs.as_deref(), Some(b"/rootfs".as_slice()));
        assert_eq!(spec.plan.arguments[0], b"/bin/true");
        assert_eq!(spec.plan.options.get("HL_NETNS"), Some("container-test"));
    }
}
