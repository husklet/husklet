use super::*;

/// Per-container host scratch dir backing a `--tmpfs`/`--mount type=tmpfs` mount at `target`. A plain
/// host dir (path-spliced over the guest target like a bind); it is cleared fresh on every container
/// start (see [`clear_tmpfs`]), so the guest sees an empty mount each run — the "in-memory tmpfs" contract
/// that matters to callers. Keyed by CONTAINER id (an exec passes the container's id via `netns_key`) so
/// an exec into the container sees the same tmpfs. Size/mode options are metadata only (not a real tmpfs).
pub(crate) struct Tmpfs;
impl Tmpfs {
    pub(crate) fn host_dir(cid: &str, target: &str) -> String {
        let slug: String = target
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        crate::util::hl_home()
            .join("containers")
            .join(cid)
            .join("tmpfs")
            .join(slug)
            .to_string_lossy()
            .into_owned()
    }

    /// Reset every `--tmpfs` target of a container to a FRESH empty dir. Called on each real container start
    /// (never on an exec, which must not wipe the container's live tmpfs). Best-effort.
    pub(crate) fn clear(c: &Container) {
        for target in c.tmpfs.keys() {
            let d = Self::host_dir(&c.id, target);
            let _ = std::fs::remove_dir_all(&d);
            let _ = std::fs::create_dir_all(&d);
        }
    }
}
