use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use hl_vfs::GuestPath;

/// Linux filesystem context state shared by threads and retained across exec.
#[derive(Debug)]
pub struct FsContext {
    umask: AtomicU32,
    root: Mutex<GuestPath>,
}

impl FsContext {
    #[must_use]
    pub fn new(mask: u32) -> Self {
        Self {
            umask: AtomicU32::new(mask & 0o777),
            root: Mutex::new(GuestPath::new("/").expect("root is a valid guest path")),
        }
    }

    #[must_use]
    pub fn mask(&self) -> u32 {
        self.umask.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn replace_mask(&self, mask: u32) -> u32 {
        self.umask.swap(mask & 0o777, Ordering::AcqRel)
    }

    #[must_use]
    pub fn root(&self) -> GuestPath {
        self.root.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub fn replace_root(&self, path: GuestPath) {
        *self.root.lock().unwrap_or_else(|error| error.into_inner()) = path;
    }

    pub fn rooted(&self, path: &GuestPath) -> Result<GuestPath, ()> {
        let root = self.root();
        if root.as_str() == "/" {
            return Ok(path.clone());
        }
        let mut combined = root.as_str().trim_end_matches('/').to_owned();
        combined.push('/');
        combined.push_str(path.as_str().trim_start_matches('/'));
        GuestPath::new(&combined).map_err(|_| ())
    }

    pub fn guest_path(&self, path: &GuestPath) -> GuestPath {
        let root = self.root();
        if root.as_str() == "/" {
            return path.clone();
        }
        let Some(suffix) = path.as_str().strip_prefix(root.as_str()).filter(|suffix| suffix.is_empty() || suffix.starts_with('/')) else {
            return GuestPath::new("/").expect("root is a valid guest path");
        };
        GuestPath::new(if suffix.is_empty() { "/" } else { suffix }).expect("a suffix of a valid guest path is valid")
    }

    #[must_use]
    pub fn fork_copy(&self) -> Self {
        Self {
            umask: AtomicU32::new(self.mask()),
            root: Mutex::new(self.root()),
        }
    }

    #[must_use]
    pub fn apply(&self, mode: u32) -> u32 {
        mode & !self.mask()
    }
}

impl Default for FsContext {
    fn default() -> Self {
        Self::new(0o022)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_copy_detaches_root_and_mask() {
        let parent = FsContext::new(0o022);
        parent.replace_root(GuestPath::new("/sandbox").unwrap());

        let child = parent.fork_copy();
        child.replace_root(GuestPath::new("/sandbox/child").unwrap());
        let _ = child.replace_mask(0o077);

        assert_eq!(parent.root().as_str(), "/sandbox");
        assert_eq!(parent.mask(), 0o022);
        assert_eq!(child.root().as_str(), "/sandbox/child");
        assert_eq!(child.mask(), 0o077);
    }

    #[test]
    fn shared_context_publishes_root_changes() {
        let context = std::sync::Arc::new(FsContext::default());
        let peer = std::sync::Arc::clone(&context);
        peer.replace_root(GuestPath::new("/empty").unwrap());
        assert_eq!(context.root().as_str(), "/empty");
    }

    #[test]
    fn rooted_paths_are_confined_and_return_to_guest_frame() {
        let context = FsContext::default();
        context.replace_root(GuestPath::new("/sandbox").unwrap());
        let rooted = context.rooted(&GuestPath::new("/work/../tmp").unwrap()).unwrap();
        assert_eq!(rooted.as_str(), "/sandbox/tmp");
        assert_eq!(context.guest_path(&rooted).as_str(), "/tmp");
        assert_eq!(context.guest_path(&GuestPath::new("/outside").unwrap()).as_str(), "/");
    }
}
