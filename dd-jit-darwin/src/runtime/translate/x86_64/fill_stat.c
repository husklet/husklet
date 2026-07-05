// frontend/x86_64/fill_stat.c -- the x86-64 Linux `struct stat` byte layout (per-arch; differs from
// aarch64). Provided to the shared os/linux/ (vfs synth + the stat syscalls), which stay layout-agnostic.

// container uid/gid virtualization (cuid/cgid in os/linux/container/state.c, included later in the TU):
// report files owned by the engine's REAL host uid as the container's uid/gid so guest ownership checks
// pass (postgres initdb "data directory has wrong ownership" under a non-root DD_UID).
static int cuid(void);
static int cgid(void);
// a guest chown is persisted as a host xattr on the overlay-upper file; prefer it over the
// cuid/cgid default (defined in os/linux/container/state.c, later in this unity TU). hostpath/fd
// identify the just-stat'd backing file (NULL/-1 when synthetic or unavailable -> default applies).
static int chown_xattr_get(const char *hostpath, int fd, uint64_t dev, uint64_t ino, int *uid, int *gid);
// shared owner virtualization (cuid/cgid default + guest-chown xattr override via the
// cache), defined in os/linux/container/state.c later in the unity TU. statx uses it too, so every
// stat-family syscall reports identical ownership for the same file.
static void stat_virt_ids(const struct stat *s, const char *hostpath, int fd, uint32_t *uid, uint32_t *gid);

static void fill_linux_stat(uint8_t *d, const struct stat *s, const char *hostpath, int fd) {
    memset(d, 0, 144);
    *(uint64_t *)(d + 0) = s->st_dev;
    *(uint64_t *)(d + 8) = s->st_ino;
    *(uint64_t *)(d + 16) = s->st_nlink;
    *(uint32_t *)(d + 24) = s->st_mode;
    uint32_t uid, gid;
    stat_virt_ids(s, hostpath, fd, &uid, &gid);
    *(uint32_t *)(d + 28) = uid;
    *(uint32_t *)(d + 32) = gid;
    *(uint64_t *)(d + 40) = s->st_rdev;
    *(uint64_t *)(d + 48) = s->st_size;
    *(uint64_t *)(d + 56) = 4096;
    *(uint64_t *)(d + 64) = s->st_blocks;
    *(uint64_t *)(d + 72) = s->st_atime;  // atime sec
    *(uint64_t *)(d + 88) = s->st_mtime;  // mtime sec
    *(uint64_t *)(d + 104) = s->st_ctime; // ctime sec
}
