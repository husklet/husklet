use super::{AuthorityWorker, ProjectionError};

impl AuthorityWorker {
    pub fn tree_open_options(
        &mut self,
        base: u64,
        path: &[u8],
        options: hl_provider::TreeOpen,
    ) -> Result<u64, ProjectionError> {
        let request = hl_provider::TreeWire::open_options(base, path, options).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::open_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_open(&mut self, path: &[u8], directory: bool) -> Result<u64, ProjectionError> {
        let request = hl_provider::TreeWire::open(path, directory).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::open_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_open_link(&mut self, path: &[u8]) -> Result<u64, ProjectionError> {
        let request = hl_provider::TreeWire::open_link(path).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::open_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_open_at(&mut self, base: u64, path: &[u8], directory: bool) -> Result<u64, ProjectionError> {
        let request = hl_provider::TreeWire::open_at(base, path, directory).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::open_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_link_at(&mut self, base: u64, path: &[u8]) -> Result<u64, ProjectionError> {
        let request = hl_provider::TreeWire::open_link_at(base, path).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::open_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_read(&mut self, handle: u64, offset: u64, size: usize) -> Result<Vec<u8>, ProjectionError> {
        let request = hl_provider::TreeWire::read(handle, offset, size).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::read_reply(&reply, size).map_err(ProjectionError::Linux)
    }
    pub fn tree_stat(&mut self, handle: u64) -> Result<hl_provider::TreeStat, ProjectionError> {
        let reply = self.provider(&hl_provider::TreeWire::stat(handle))?;
        hl_provider::TreeWire::stat_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_read_link(&mut self, handle: u64, size: usize) -> Result<Vec<u8>, ProjectionError> {
        let request = hl_provider::TreeWire::link(handle, size).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::link_reply(&reply, size).map_err(ProjectionError::Linux)
    }
    pub fn tree_entries(&mut self, handle: u64, size: usize) -> Result<Vec<u8>, ProjectionError> {
        let request = hl_provider::TreeWire::dents(handle, size).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::dents_reply(&reply, size).map_err(ProjectionError::Linux)
    }
    pub fn tree_close(&mut self, handle: u64) -> Result<(), ProjectionError> {
        let reply = self.provider(&hl_provider::TreeWire::close(handle))?;
        hl_provider::TreeWire::close_reply(&reply).map_err(ProjectionError::Linux)
    }
    pub fn tree_write(&mut self, handle: u64, offset: u64, input: &[u8]) -> Result<usize, ProjectionError> {
        let request = hl_provider::TreeWire::write(handle, offset, input).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::write_reply(&reply, input.len()).map_err(ProjectionError::Linux)
    }
    pub fn tree_append(&mut self, handle: u64, input: &[u8]) -> Result<(usize, u64), ProjectionError> {
        let request = hl_provider::TreeWire::append(handle, input).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::TreeWire::append_reply(&reply, input.len()).map_err(ProjectionError::Linux)
    }
    pub fn tree_truncate(&mut self, handle: u64, size: u64) -> Result<(), ProjectionError> {
        let reply = self.provider(&hl_provider::TreeWire::truncate(handle, size))?;
        hl_provider::TreeWire::truncate_reply(&reply).map_err(ProjectionError::Linux)
    }
}
