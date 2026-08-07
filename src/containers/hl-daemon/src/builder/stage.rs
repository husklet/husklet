use super::context::Context;
use super::copy::copy_root;
use super::remote::RemoteSources;
use super::{BaseState, Build, BuildError, Builder};
use hl_images::RuntimeOverrides;
use hl_images::build::{Base, Recipe, Stage};
use std::path::Path;

impl Builder {
    pub(super) async fn stage(
        &self,
        context: &Context<'_>,
        stage: &Stage,
        built: &[Build],
        images: &hl_images::Images,
        remotes: &RemoteSources,
    ) -> Result<Build, BuildError> {
        let root = tempfile::tempdir()?;
        let BaseState {
            base,
            mut labels,
            mut history,
            triggers,
            mut exposed_ports,
            mut volumes,
            mut healthcheck,
            mut stop_signal,
            mut ownerships,
        } = self.base(stage, built, images, root.path())?;
        let trigger = if triggers.is_empty() {
            None
        } else {
            let dockerfile = format!("FROM scratch\n{}\n", triggers.join("\n"));
            Some(Recipe::parse(&dockerfile)?.stages.remove(0))
        };
        let triggered = trigger
            .as_ref()
            .map_or_else(|| Ok(base.clone()), |trigger| base.merge(trigger.overrides()))?;
        let runtime = triggered.merge(RuntimeOverrides {
            command: stage.command.clone(),
            entrypoint: stage.entrypoint.clone(),
            environment: stage.runtime.environment.clone(),
            working_directory: stage.working_directory.clone(),
            user: stage.user.clone(),
        })?;
        if let Some(trigger) = &trigger {
            labels.extend(trigger.labels.clone());
            history.extend(trigger.history.iter().skip(1).cloned());
            exposed_ports.extend(trigger.exposed_ports.clone());
            volumes.extend(trigger.volumes.clone());
            if trigger.healthcheck.is_some() {
                trigger.healthcheck.clone_into(&mut healthcheck);
            }
        }
        labels.extend(stage.labels.clone());
        history.extend(stage.history.clone());
        exposed_ports.extend(stage.exposed_ports.clone());
        volumes.extend(stage.volumes.clone());
        if stage.healthcheck.is_some() {
            stage.healthcheck.clone_into(&mut healthcheck);
        }
        if stage.stop_signal.is_some() {
            stage.stop_signal.clone_into(&mut stop_signal);
        }
        let mut steps = trigger
            .iter()
            .flat_map(|trigger| trigger.steps.iter().cloned().map(|step| (step, base.clone())))
            .collect::<Vec<_>>();
        steps.extend(stage.steps.iter().cloned().map(|step| (step, triggered.clone())));
        self.apply_steps(context, built, root.path(), steps, &mut ownerships, remotes)
            .await?;
        Ok(Build {
            root,
            runtime,
            labels,
            history,
            onbuild: stage.onbuild.clone(),
            exposed_ports,
            volumes,
            healthcheck,
            stop_signal,
            ownerships,
        })
    }

    fn base(
        &self,
        stage: &Stage,
        built: &[Build],
        images: &hl_images::Images,
        root: &Path,
    ) -> Result<BaseState, BuildError> {
        match &stage.base {
            Base::Image(reference) => {
                let image = images
                    .resolve(reference)?
                    .ok_or_else(|| hl_images::Error::InvalidMetadata(format!("base image {reference} is not local")))?;
                let unpacked = images.unpack(&image, &self.platform)?;
                let owned = images.rootfs(&unpacked)?;
                let view = images.roots().open(&owned)?;
                let ownerships = copy_root(view.path(), view.ownership(), root)?;
                images.roots().release(&owned)?;
                let metadata = images.details(&image, &self.platform)?;
                Ok(BaseState {
                    base: unpacked.runtime().clone(),
                    labels: metadata.labels,
                    history: metadata.history,
                    triggers: metadata.onbuild,
                    exposed_ports: metadata.exposed_ports,
                    volumes: metadata.volumes,
                    healthcheck: metadata.healthcheck,
                    stop_signal: metadata.stop_signal,
                    ownerships,
                })
            }
            Base::Stage(index) => {
                let source = built
                    .get(*index)
                    .ok_or_else(|| hl_images::Error::MalformedOci("stage depends on an unavailable stage".into()))?;
                let ownerships = copy_root(source.root.path(), &source.ownerships, root)?;
                Ok(BaseState {
                    base: source.runtime.clone(),
                    labels: source.labels.clone(),
                    history: source.history.clone(),
                    triggers: source.onbuild.clone(),
                    exposed_ports: source.exposed_ports.clone(),
                    volumes: source.volumes.clone(),
                    healthcheck: source.healthcheck.clone(),
                    stop_signal: source.stop_signal.clone(),
                    ownerships,
                })
            }
        }
    }
}
