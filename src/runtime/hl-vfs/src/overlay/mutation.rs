use crate::{
    CopyContent, CopyUpPlan, CreatePlan, GuestPathBytes, Layer, MutationHandle, NodeMetadata, Overlay, OverlayError,
    OverlayHost, OverlayLookup, OverlayNodeKind,
};

impl<H: OverlayHost> Overlay<H> {
    /// Builds a copy-up plan without mutating the upper layer.
    pub fn plan_copy_up(&self, path: &GuestPathBytes, read_only: bool) -> Result<CopyUpPlan, OverlayError> {
        if read_only {
            return Err(OverlayError::ReadOnly);
        }
        let normalized = path.normalize_absolute().map_err(|_| OverlayError::InvalidPath)?;
        let OverlayLookup::Present { layer, kind, metadata } = self.lookup(&normalized)? else {
            return Err(OverlayError::NotFound);
        };
        if layer == Layer::Upper {
            return Err(OverlayError::AlreadyUpper);
        }
        let content = match kind {
            OverlayNodeKind::Regular => CopyContent::Regular { size: metadata.size },
            OverlayNodeKind::Symlink => CopyContent::Symlink,
            OverlayNodeKind::Directory | OverlayNodeKind::Other => CopyContent::None,
        };
        hl_log::hl_debug!(
            hl_log::tag::FS,
            "overlay copy_up plan path={} source={layer:?} kind={kind:?} size={}",
            normalized.as_bytes().escape_ascii(),
            metadata.size
        );
        Ok(CopyUpPlan {
            path: normalized,
            source: layer,
            kind,
            metadata,
            content,
        })
    }

    /// Builds a create plan after merged-view existence and parent checks.
    pub fn plan_create(
        &self,
        path: &GuestPathBytes,
        kind: OverlayNodeKind,
        metadata: NodeMetadata,
        read_only: bool,
    ) -> Result<CreatePlan, OverlayError> {
        if read_only {
            return Err(OverlayError::ReadOnly);
        }
        let normalized = path.normalize_absolute().map_err(|_| OverlayError::InvalidPath)?;
        if !matches!(self.lookup(&normalized)?, OverlayLookup::Absent { .. }) {
            return Err(OverlayError::AlreadyExists);
        }
        let parent = Self::parent_path(&normalized)?;
        let OverlayLookup::Present { kind: parent_kind, .. } = self.lookup(&parent)? else {
            return Err(OverlayError::NotFound);
        };
        if parent_kind != OverlayNodeKind::Directory {
            return Err(OverlayError::NotDirectory);
        }
        Ok(CreatePlan {
            path: normalized,
            kind,
            metadata,
        })
    }

    /// Stages and atomically publishes one copy-up.
    pub fn copy_up(&self, plan: &CopyUpPlan) -> Result<(), OverlayError> {
        let mut mutation = Mutation::begin(&self.host, &plan.path)?;
        mutation.stage_parents(&plan.path)?;
        mutation.stage_copy(plan)?;
        mutation.commit()
    }

    /// Stages and atomically publishes one new upper entry.
    pub fn create(&self, plan: &CreatePlan) -> Result<(), OverlayError> {
        let mut mutation = Mutation::begin(&self.host, &plan.path)?;
        mutation.stage_parents(&plan.path)?;
        mutation.stage_create(plan)?;
        mutation.commit()
    }

    fn parent_path(path: &GuestPathBytes) -> Result<GuestPathBytes, OverlayError> {
        path.parent_and_name()
            .map(|(parent, _)| parent)
            .map_err(|_| OverlayError::InvalidPath)
    }
}

struct Mutation<'host, H: OverlayHost> {
    host: &'host H,
    handle: MutationHandle,
    active: bool,
}

impl<'host, H: OverlayHost> Mutation<'host, H> {
    fn begin(host: &'host H, path: &GuestPathBytes) -> Result<Self, OverlayError> {
        let handle = host.begin_mutation(path).map_err(OverlayError::Host)?;
        hl_log::hl_debug!(
            hl_log::tag::FS,
            "overlay mutation begin handle={handle:?} path={}",
            path.as_bytes().escape_ascii()
        );
        Ok(Self {
            host,
            handle,
            active: true,
        })
    }

    fn stage_parents(&mut self, path: &GuestPathBytes) -> Result<(), OverlayError> {
        self.host
            .stage_parent_directories(self.handle, path)
            .map_err(OverlayError::Host)
    }

    fn stage_copy(&mut self, plan: &CopyUpPlan) -> Result<(), OverlayError> {
        self.host.stage_copy_up(self.handle, plan).map_err(OverlayError::Host)
    }

    fn stage_create(&mut self, plan: &CreatePlan) -> Result<(), OverlayError> {
        self.host.stage_create(self.handle, plan).map_err(OverlayError::Host)
    }

    fn commit(mut self) -> Result<(), OverlayError> {
        self.host.commit_mutation(self.handle).map_err(OverlayError::Host)?;
        hl_log::hl_debug!(hl_log::tag::FS, "overlay mutation commit handle={:?}", self.handle);
        self.active = false;
        Ok(())
    }
}

impl<H: OverlayHost> Drop for Mutation<'_, H> {
    fn drop(&mut self) {
        if self.active {
            hl_log::hl_warn!(hl_log::tag::FS, "overlay mutation rollback handle={:?}", self.handle);
            self.host.rollback_mutation(self.handle);
        }
    }
}
