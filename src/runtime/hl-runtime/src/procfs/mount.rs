use hl_vfs::{MountKind, MountSnapshot, ProcfsError, ProcfsMountEntry, ProcfsMountView};

/// Consumer port for the live mount namespace owned by the filesystem domain.
pub trait MountPort: Send + Sync {
    fn snapshot(&self) -> Vec<MountSnapshot>;
}

impl MountPort for hl_vfs::MountNamespace {
    fn snapshot(&self) -> Vec<MountSnapshot> {
        hl_vfs::MountNamespace::snapshot(self)
    }
}

fn entry(
    id: u32,
    parent: u32,
    device: (u32, u32),
    point: &[u8],
    options: &[&[u8]],
    filesystem: &[u8],
    source: &[u8],
    super_options: &[&[u8]],
) -> Result<ProcfsMountEntry, ProcfsError> {
    ProcfsMountEntry::new(
        id,
        parent,
        device,
        b"/".to_vec(),
        point.to_vec(),
        options.iter().map(|value| value.to_vec()).collect(),
        Vec::new(),
        filesystem.to_vec(),
        source.to_vec(),
        super_options.iter().map(|value| value.to_vec()).collect(),
    )
    .ok_or(ProcfsError::Invalid)
}

impl super::TaskProcfs {
    /// Returns the retained engine's fixed base mount namespace plus live
    /// directory mounts supplied by the filesystem domain.
    pub(super) fn mount_view(mounts: &[MountSnapshot]) -> Result<ProcfsMountView, ProcfsError> {
        let mut entries = vec![
            entry(
                23,
                0,
                (0, 24),
                b"/",
                &[b"rw", b"relatime"],
                b"overlay",
                b"overlay",
                &[b"rw"],
            )?,
            entry(
                24,
                23,
                (0, 25),
                b"/proc",
                &[b"rw", b"nosuid", b"nodev", b"noexec", b"relatime"],
                b"proc",
                b"proc",
                &[b"rw"],
            )?,
            entry(
                25,
                23,
                (0, 26),
                b"/dev",
                &[b"rw", b"nosuid"],
                b"tmpfs",
                b"tmpfs",
                &[b"rw", b"size=65536k", b"mode=755"],
            )?,
            entry(
                26,
                25,
                (0, 27),
                b"/dev/pts",
                &[b"rw", b"nosuid", b"noexec", b"relatime"],
                b"devpts",
                b"devpts",
                &[b"rw", b"gid=5", b"mode=620", b"ptmxmode=666"],
            )?,
            entry(
                27,
                23,
                (0, 28),
                b"/sys",
                &[b"ro", b"nosuid", b"nodev", b"noexec", b"relatime"],
                b"sysfs",
                b"sysfs",
                &[b"ro"],
            )?,
            entry(
                28,
                27,
                (0, 29),
                b"/sys/fs/cgroup",
                &[b"ro", b"nosuid", b"nodev", b"noexec", b"relatime"],
                b"cgroup2",
                b"cgroup",
                &[b"rw", b"nsdelegate"],
            )?,
            entry(
                29,
                25,
                (0, 30),
                b"/dev/mqueue",
                &[b"rw", b"nosuid", b"nodev", b"noexec", b"relatime"],
                b"mqueue",
                b"mqueue",
                &[b"rw"],
            )?,
            entry(
                30,
                25,
                (0, 31),
                b"/dev/shm",
                &[b"rw", b"nosuid", b"nodev", b"noexec", b"relatime"],
                b"tmpfs",
                b"shm",
                &[b"rw", b"size=65536k"],
            )?,
        ];
        let base = [
            b"/dev/shm".as_slice(),
            b"/proc",
            b"/dev",
            b"/dev/pts",
            b"/sys",
            b"/sys/fs/cgroup",
            b"/dev/mqueue",
        ];
        let mut id = 100_u32;
        for mount in mounts.iter().filter(|mount| {
            mount.active && mount.kind == MountKind::Directory && !base.contains(&mount.guest_path.as_str().as_bytes())
        }) {
            let access = if mount.read_only {
                b"ro".as_slice()
            } else {
                b"rw".as_slice()
            };
            entries.push(entry(
                id,
                23,
                (254, 1),
                mount.guest_path.as_str().as_bytes(),
                &[access, b"relatime"],
                b"ext4",
                b"/dev/root",
                &[access],
            )?);
            id = id.checked_add(1).ok_or(ProcfsError::Invalid)?;
        }
        ProcfsMountView::new(entries).ok_or(ProcfsError::Invalid)
    }
}
