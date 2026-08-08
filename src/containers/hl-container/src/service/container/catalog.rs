use super::{Container, ContainerId, ContainerSpec, ContainerState, Error, Ordering, Result, Service, now_ms};

impl Service {
    pub(crate) async fn create(&self, mut spec: ContainerSpec) -> Result<Container> {
        spec.validate()?;
        self.rootfs_path(&spec.rootfs).await?;
        let _guard = self.operations.lock().await;
        self.allocate_ports(&mut spec).await?;
        self.volumes.validate(&spec.mounts).await?;
        if let Some(name) = &spec.name {
            self.ensure_name_available(name, None).await?;
        }
        let container = Container::new(
            ContainerId::new(),
            spec,
            ContainerState::Created,
            self.next_created_ms(),
        );
        self.containers.insert(&container).await?;
        let mut exits = self.exits.lock().await;
        exits.remove(container.id.as_str());
        if let Some(name) = &container.spec.name {
            exits.remove(name);
        }
        drop(exits);
        self.emit(crate::LifecycleAction::Create, &container);
        Ok(container)
    }

    async fn allocate_ports(&self, spec: &mut ContainerSpec) -> Result<()> {
        let mut used = self
            .containers
            .list()
            .await?
            .into_iter()
            .flat_map(|container| container.spec.publish)
            .filter(|publish| publish.host != 0)
            .collect::<Vec<_>>();
        used.extend(spec.publish.iter().copied().filter(|publish| publish.host != 0));
        for publish in &mut spec.publish {
            if publish.host != 0 {
                continue;
            }
            let Some(host) = (49152..=65535).find(|host| {
                let candidate = Self::publication(*publish, *host);
                !used.iter().any(|existing| existing.conflicts(candidate))
            }) else {
                return Err(Error::InvalidSpec("no automatic host TCP ports are available".into()));
            };
            publish.host = host;
            used.push(*publish);
        }
        Ok(())
    }

    fn publication(request: crate::Publication, host: u16) -> crate::Publication {
        crate::Publication { host, ..request }
    }

    pub(crate) async fn list(&self) -> Result<Vec<Container>> {
        let mut values = self.containers.list().await?;
        values.sort_by_key(|item| (item.created_at_ms, item.id.clone()));
        Ok(values)
    }

    pub(crate) async fn inspect(&self, reference: &str) -> Result<Container> {
        self.resolve(reference).await
    }

    pub(crate) async fn set_label(&self, reference: &str, name: String, value: String) -> Result<Container> {
        if name.is_empty() {
            return Err(Error::InvalidSpec("label name must not be empty".into()));
        }
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        container.spec.labels.insert(name, value);
        container.spec.validate()?;
        self.containers.replace(&container).await?;
        Ok(container)
    }

    pub(crate) fn images(&self) -> Option<hl_images::Images> {
        self.images.clone()
    }

    pub(crate) async fn rename(&self, reference: &str, name: String) -> Result<Container> {
        if name.is_empty() {
            return Err(Error::InvalidSpec("name must not be empty".into()));
        }
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        let old = container.spec.name.clone();
        container.spec.name = Some(name);
        container.spec.validate()?;
        self.ensure_name_available(
            container.spec.name.as_deref().expect("rename assigned a name"),
            Some(&container.id),
        )
        .await?;
        if let Some(old) = old.as_deref() {
            self.networks
                .rename_generated_endpoint(&container.id, old, container.spec.name.as_deref().expect("assigned"))
                .await?;
        }
        if let Err(error) = self.containers.replace(&container).await {
            if let Some(old) = old.as_deref()
                && let Err(rollback) = self
                    .networks
                    .rename_generated_endpoint(&container.id, container.spec.name.as_deref().expect("assigned"), old)
                    .await
            {
                return Err(Error::Corrupt(format!(
                    "rename failed ({error}); network-name rollback also failed ({rollback})"
                )));
            }
            return Err(error);
        }
        Ok(container)
    }

    pub(crate) async fn update(&self, reference: &str, update: crate::Update) -> Result<Container> {
        let _guard = self.operations.lock().await;
        let mut container = self.resolve(reference).await?;
        let previous = container.spec.resources.clone();
        update.apply(&mut container.spec.resources, &mut container.spec.restart);
        container.spec.validate()?;
        let cancel_restart = match container.state {
            ContainerState::Restarting {
                result, finished_at_ms, ..
            } if !container.spec.restart.allows(result, &container.restart) => {
                container.state = ContainerState::Exited { result, finished_at_ms };
                true
            }
            _ => false,
        };
        if container.state.is_active() && container.spec.resources != previous {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "stopped before changing resource limits",
            });
        }
        self.containers.replace(&container).await?;
        if cancel_restart {
            if let Some(cancel) = self.restarts.lock().await.remove(&container.id) {
                let _ = cancel.send(true);
            }
            if let Some(notify) = self.waiters.lock().await.get(&container.id) {
                notify.notify_waiters();
            }
        }
        Ok(container)
    }

    async fn ensure_name_available(&self, name: &str, except: Option<&ContainerId>) -> Result<()> {
        if self
            .containers
            .list()
            .await?
            .iter()
            .any(|item| item.spec.name.as_deref() == Some(name) && except != Some(&item.id))
        {
            return Err(Error::NameConflict(name.into()));
        }
        Ok(())
    }

    fn next_created_ms(&self) -> u64 {
        let now = now_ms();
        self.last_created_ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |previous| {
                Some(now.max(previous.saturating_add(1)))
            })
            .map_or(now, |previous| now.max(previous.saturating_add(1)))
    }

    pub(super) async fn required(&self, id: &ContainerId) -> Result<Container> {
        self.containers
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    pub(super) async fn resolve(&self, reference: &str) -> Result<Container> {
        let reference = reference.trim_start_matches('/');
        if reference.is_empty() {
            return Err(Error::InvalidSpec("container reference must not be empty".into()));
        }
        let mut matches = self.containers.list().await?.into_iter().filter(|item| {
            item.id.as_str() == reference
                || item.spec.name.as_deref() == Some(reference)
                || item.id.as_str().starts_with(reference)
        });
        let first = matches.next().ok_or_else(|| Error::NotFound(reference.into()))?;
        if matches.next().is_some() {
            return Err(Error::InvalidSpec(format!(
                "container reference {reference:?} is ambiguous"
            )));
        }
        Ok(first)
    }
}
