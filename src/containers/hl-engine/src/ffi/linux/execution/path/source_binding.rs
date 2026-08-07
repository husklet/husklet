//! Bounded live name-bind aliases layered over the ordinary path source.

use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;

use hl_runtime::{GuestName, GuestPath, GuestPathBytes, MountRoute, ResolveRequest, Resolver, RuntimePathError};

use super::super::HostError;
use super::OrdinaryContext;

const NAME_BIND_MAXIMUM: usize = 32;
const NAME_ALIAS_MAXIMUM: usize = 8;

#[derive(Debug)]
pub(super) struct NameBind {
    aliases: Vec<GuestName>,
    exact: Vec<GuestPath>,
    read_only: bool,
    host: PathBuf,
    parent: Arc<File>,
    leaf: CString,
}

/// One resolved live alias. The pinned host parent preserves the C oracle's
/// rename-safe lookup identity while each open still observes the current leaf.
#[derive(Clone, Debug)]
pub(in crate::ffi::linux::execution::path) struct NameBinding {
    pub(in crate::ffi::linux::execution::path) guest: GuestPath,
    pub(in crate::ffi::linux::execution::path) host: PathBuf,
    pub(in crate::ffi::linux::execution::path) parent: Arc<File>,
    pub(in crate::ffi::linux::execution::path) leaf: CString,
    pub(in crate::ffi::linux::execution::path) read_only: bool,
}

impl OrdinaryContext {
    pub(in crate::ffi::linux::execution) fn add_name_binds(&self, spec: &str) -> Result<(), RuntimePathError> {
        if spec.is_empty() {
            return Ok(());
        }
        let mut parsed = Vec::new();
        for record in spec.split('\n') {
            if parsed.len() == NAME_BIND_MAXIMUM {
                return Err(RuntimePathError::TooLarge);
            }
            let mut fields = record.split('\t');
            let host = fields
                .next()
                .filter(|value| !value.is_empty())
                .ok_or(RuntimePathError::Invalid)?;
            let (host, read_only) = host.strip_prefix("rw:").map_or_else(
                || (host.strip_prefix("ro:").unwrap_or(host), true),
                |host| (host, false),
            );
            let host = PathBuf::from(host).canonicalize().map_err(HostError::map)?;
            if !host.is_file() {
                return Err(RuntimePathError::Invalid);
            }
            let parent_path = host.parent().ok_or(RuntimePathError::Invalid)?;
            let parent = Arc::new(File::open(parent_path).map_err(HostError::map)?);
            let leaf = host
                .file_name()
                .ok_or(RuntimePathError::Invalid)
                .and_then(|name| CString::new(name.as_bytes()).map_err(|_| RuntimePathError::Invalid))?;
            let mut aliases = Vec::new();
            let mut exact = Vec::new();
            for field in fields {
                if aliases.len() + exact.len() == NAME_ALIAS_MAXIMUM {
                    return Err(RuntimePathError::TooLarge);
                }
                if !field.starts_with('/') {
                    let alias = GuestName::new(field.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
                    if aliases.contains(&alias) {
                        return Err(RuntimePathError::Invalid);
                    }
                    aliases.push(alias);
                    continue;
                }
                let path = GuestPath::new(field).map_err(|_| RuntimePathError::Invalid)?;
                if exact.contains(&path) {
                    return Err(RuntimePathError::Invalid);
                }
                exact.push(path);
            }
            if aliases.is_empty() && exact.is_empty() {
                return Err(RuntimePathError::Invalid);
            }
            parsed.push(NameBind {
                aliases,
                exact,
                read_only,
                host,
                parent,
                leaf,
            });
        }
        *self
            .name_binds
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = parsed;
        Ok(())
    }

    pub(in crate::ffi::linux::execution::path) fn name_binding(
        &self,
        base: &GuestPath,
        raw: &[u8],
    ) -> Result<Option<NameBinding>, RuntimePathError> {
        let guest = Self::absolute_guest(base, raw)?;
        let guest_bytes = GuestPathBytes::new(guest.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        if !matches!(self.mounts.route_bytes(&guest_bytes), MountRoute::Root) {
            return Ok(None);
        }
        let leaf = guest_bytes
            .as_bytes()
            .rsplit(|byte| *byte == b'/')
            .next()
            .ok_or(RuntimePathError::Invalid)?;
        let parent = guest
            .as_str()
            .rfind('/')
            .map(|slash| if slash == 0 { "/" } else { &guest.as_str()[..slash] })
            .ok_or(RuntimePathError::Invalid)?;
        let rules = self
            .name_binds
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(rule) = rules.iter().find(|rule| rule.exact.contains(&guest)) {
            return Ok(Some(NameBinding {
                guest,
                host: rule.host.clone(),
                parent: Arc::clone(&rule.parent),
                leaf: rule.leaf.clone(),
                read_only: rule.read_only,
            }));
        }
        for rule in rules
            .iter()
            .filter(|rule| rule.aliases.iter().any(|alias| alias.as_bytes() == leaf))
        {
            if self.alias_sibling_present(rule, parent)? {
                return Ok(Some(NameBinding {
                    guest,
                    host: rule.host.clone(),
                    parent: Arc::clone(&rule.parent),
                    leaf: rule.leaf.clone(),
                    read_only: rule.read_only,
                }));
            }
        }
        Ok(None)
    }

    /// Reports whether any alias of the rule names a non-directory sibling.
    fn alias_sibling_present(&self, rule: &NameBind, parent: &str) -> Result<bool, RuntimePathError> {
        for alias in &rule.aliases {
            let sibling = if parent == "/" {
                format!("/{}", String::from_utf8_lossy(alias.as_bytes()))
            } else {
                format!("{parent}/{}", String::from_utf8_lossy(alias.as_bytes()))
            };
            let sibling = GuestPathBytes::new(sibling.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
            if self.raw_non_directory(&sibling)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn raw_non_directory(&self, guest: &GuestPathBytes) -> Result<bool, RuntimePathError> {
        let resolver = Resolver::new(self.host(), self.mounts());
        let root = GuestPathBytes::new(b"/").map_err(|_| RuntimePathError::Invalid)?;
        let Ok(resolved) = resolver.resolve(ResolveRequest {
            path: guest,
            base: &root,
            nofollow_final: true,
            no_symlinks: false,
            allow_missing_final: false,
        }) else {
            return Ok(false);
        };
        let Some(name) = resolved.final_name() else {
            return Ok(false);
        };
        let name = CString::new(name.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let parent = resolved.duplicate_parent().map_err(|_| RuntimePathError::Invalid)?;
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the duplicated parent is live, the name is terminated,
        // and fstatat initializes status on success without retaining pointers.
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Ok(false);
        }
        // SAFETY: successful fstatat initialized the status object.
        Ok(unsafe { status.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR)
    }

    fn absolute_guest(base: &GuestPath, raw: &[u8]) -> Result<GuestPath, RuntimePathError> {
        let raw = std::str::from_utf8(raw).map_err(|_| RuntimePathError::Invalid)?;
        let combined = if raw.starts_with('/') {
            raw.to_owned()
        } else {
            format!("{}/{}", base.as_str().trim_end_matches('/'), raw)
        };
        GuestPath::new(&combined).map_err(|_| RuntimePathError::Invalid)
    }
}

#[cfg(test)]
mod name_bind_tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hl-name-bind-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn aliases_follow_live_siblings_and_mounts_win() {
        let root = fixture("root");
        let directory = root.join("dir");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("probe.so.1"), b"guest").unwrap();
        let host = fixture("host");
        let projected = host.join("projected");
        std::fs::write(&projected, b"projected").unwrap();
        let context = OrdinaryContext::new(root.as_os_str().as_bytes()).unwrap();
        context
            .add_name_binds(&format!("{}\tprobe.so\tprobe.so.1\tprobe.so.2", projected.display()))
            .unwrap();
        let base = GuestPath::new("/").unwrap();
        let binding = context.name_binding(&base, b"/dir/probe.so").unwrap().unwrap();
        assert_eq!(binding.guest.as_str(), "/dir/probe.so");
        assert_eq!(binding.host, projected);

        std::fs::remove_file(directory.join("probe.so.1")).unwrap();
        assert!(context.name_binding(&base, b"/dir/probe.so").unwrap().is_none());
        std::fs::write(directory.join("probe.so.2"), b"late").unwrap();
        assert!(context.name_binding(&base, b"dir/probe.so").unwrap().is_some());

        let exact = fixture("exact");
        context.mount_directory("/dir", exact.to_str().unwrap(), true).unwrap();
        assert!(context.name_binding(&base, b"/dir/probe.so").unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host).unwrap();
        std::fs::remove_dir_all(exact).unwrap();
    }

    #[test]
    fn validation_is_bounded_and_transactional() {
        let root = fixture("validation-root");
        let host = fixture("validation-host");
        let projected = host.join("projected");
        std::fs::write(&projected, b"value").unwrap();
        let context = OrdinaryContext::new(root.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            context.add_name_binds(&format!("{}\tbad/name", projected.display())),
            Err(RuntimePathError::Invalid)
        );
        assert_eq!(
            context.add_name_binds(&format!("{}\tdup\tdup", projected.display())),
            Err(RuntimePathError::Invalid)
        );
        assert_eq!(
            context.add_name_binds(&format!("{}\tdirectory", host.display())),
            Err(RuntimePathError::Invalid)
        );
        assert!(
            context
                .name_binding(&GuestPath::new("/").unwrap(), b"/missing/dup")
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host).unwrap();
    }

    #[test]
    fn exact_file_bind_preserves_target_and_access() {
        let root = fixture("exact-root");
        std::fs::create_dir(root.join("etc")).unwrap();
        let host = fixture("exact-host");
        let projected = host.join("hosts");
        std::fs::write(&projected, b"127.0.0.1 localhost\n").unwrap();
        let context = OrdinaryContext::new(root.as_os_str().as_bytes()).unwrap();
        context
            .add_name_binds(&format!("rw:{}\t/etc/hosts", projected.display()))
            .unwrap();
        let base = GuestPath::new("/").unwrap();
        let binding = context.name_binding(&base, b"/etc/hosts").unwrap().unwrap();
        assert_eq!(binding.guest.as_str(), "/etc/hosts");
        assert_eq!(binding.host, projected);
        assert!(!binding.read_only);
        assert!(context.name_binding(&base, b"/tmp/hosts").unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host).unwrap();
    }
}
