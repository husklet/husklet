use super::provider::ProjectedBackend;
use crate::engine::EngineError;
use crate::session::{FrameKind, Limits, Secret, accept};

pub struct Child;
struct Services {
    files: hl_provider::FileAuthority<ProjectedBackend>,
    tree: Option<hl_provider::TreeAuthority<super::provider::ProjectedTree>>,
    network: super::network::NetworkAuthority,
}

impl Services {
    fn provider(&mut self, payload: &[u8]) -> Vec<u8> {
        if !hl_provider::TreeWire::is_request(payload) {
            return self.files.dispatch(payload);
        }
        let Some(tree) = &mut self.tree else {
            return vec![0xff, 2, 0, 0, 0, 0, 0];
        };
        tree.dispatch(payload)
    }

    fn dispatch(&mut self, kind: FrameKind, payload: &[u8]) -> Result<Vec<u8>, EngineError> {
        match kind {
            FrameKind::Provider => Ok(self.provider(payload)),
            FrameKind::Network => self.network.dispatch(payload),
            _ => Ok(payload.to_vec()),
        }
    }
}

impl Child {
    pub fn run(session: i32, bootstrap: i32) -> Result<(), EngineError> {
        Self::run_projected(session, bootstrap, -1, -1, None, None, false)
    }

    pub fn run_projected(
        session: i32,
        bootstrap: i32,
        health: i32,
        transfer: i32,
        file: Option<i32>,
        root: Option<i32>,
        writable: bool,
    ) -> Result<(), EngineError> {
        let mut session =
            crate::ffi::linux::InheritedStream::adopt(session).map_err(|()| EngineError::AuthorityFailed)?;
        let mut bootstrap =
            crate::ffi::linux::InheritedStream::adopt(bootstrap).map_err(|()| EngineError::AuthorityFailed)?;
        let secret = Secret::receive(&mut bootstrap).map_err(|_| EngineError::AuthorityFailed)?;
        drop(bootstrap);
        let _health = crate::ffi::linux::InheritedStream::adopt(health).map_err(|()| EngineError::AuthorityFailed)?;
        let transfer =
            crate::ffi::linux::InheritedDatagram::adopt(transfer).map_err(|()| EngineError::AuthorityFailed)?;
        let backend = ProjectedBackend::new(file, root)?;
        let tree = backend.tree(writable).ok();
        let mut authenticated =
            accept(&mut session, secret, Limits::new(4096, 8).unwrap()).map_err(|_| EngineError::AuthorityFailed)?;
        let limits = hl_provider::ServerLimits::new(64, hl_provider::FileWire::MAX_READ_DATA)
            .ok_or(EngineError::AuthorityFailed)?;
        let mut services = Services {
            files: hl_provider::FileAuthority::new(backend, limits),
            tree: tree
                .and_then(|backend| hl_provider::TreeAuthority::new(backend, 64, hl_provider::TreeWire::MAX_DATA)),
            network: super::network::NetworkAuthority::new(transfer),
        };
        loop {
            let frame = authenticated
                .receive(&mut session)
                .map_err(|_| EngineError::AuthorityFailed)?;
            if frame.kind == FrameKind::Close {
                return Ok(());
            }
            let payload = services.dispatch(frame.kind, &frame.payload)?;
            authenticated
                .send(&mut session, frame.kind, &payload)
                .map_err(|_| EngineError::AuthorityFailed)?;
        }
    }
}
