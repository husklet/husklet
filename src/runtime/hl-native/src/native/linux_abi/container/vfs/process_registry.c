static void set_guest_environ(const char *const *env, int envc) {
    int o = 0;
    for (int i = 0; i < envc && env && env[i]; i++) {
        int L = (int)strlen(env[i]);
        if (o + L + 1 > (int)sizeof g_self_environ) break;
        memcpy(g_self_environ + o, env[i], (size_t)L);
        o += L;
        g_self_environ[o++] = 0;
    }
    g_self_environ_len = o;
    g_self_environ_valid = 1;
}

static int proc_environ_text(char *b, size_t n) {
    int o = 0;
    // Prefer the FINAL environment build_stack placed (== getenv), so procfs and getenv agree; this includes
    // the engine defaults (HOME/LANG/GLIBC_TUNABLES) the raw HL_GUEST_ENV path below omitted.
    if (g_self_environ_valid) {
        int L = g_self_environ_len > (int)n ? (int)n : g_self_environ_len;
        memcpy(b, g_self_environ, (size_t)L);
        return L;
    }
    const char *ge = hl_process_guest_environment_get();
    if (ge && ge[0]) {
        for (const char *s = ge; *s;) {
            const char *e = s;
            while (*e && *e != '\n')
                e++;
            int L = (int)(e - s);
            if (o + L + 1 > (int)n) break;
            memcpy(b + o, s, (size_t)L);
            o += L;
            b[o++] = 0;
            s = *e ? e + 1 : e;
        }
    } else {
        static const char *const def[] = {"PATH=/usr/bin:/bin", "HOME=/root", "LANG=C",
                                          NULL}; // no TERM (docker parity: unset unless -t)
        for (int i = 0; def[i]; i++) {
            int L = (int)strlen(def[i]);
            if (o + L + 1 > (int)n) break;
            memcpy(b + o, def[i], (size_t)L);
            o += L;
            b[o++] = 0;
        }
    }
    return o;
}

// A synthesized /proc/<pid>/fd directory is backed by a REAL temp dir of "N -> target" symlinks, so the
// guest's opendir/getdents enumerate it through the ordinary fdopendir path and readlink/lstat of an
// entry resolves the symlink. The dir persists until the guest closes its fd; we reap it lazily on the
// next open (when the tracked fd is no longer open) and fully at exit.
static struct {
    int fd;
    char path[32];
} g_procfd_dirs[64];

static void procfd_dir_empty(int fd) {
    int scan = dup(fd);
    if (scan < 0) return;
    DIR *d = fdopendir(scan);
    if (!d) {
        close(scan);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.' && (!e->d_name[1] || (e->d_name[1] == '.' && !e->d_name[2]))) continue;
        if (e->d_type == DT_DIR || e->d_type == DT_UNKNOWN) {
            int child = openat(fd, e->d_name, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
            if (child >= 0) {
                procfd_dir_empty(child); // per-pid dirs nest a task/<tid>/ subtree
                close(child);
                (void)unlinkat(fd, e->d_name, AT_REMOVEDIR);
                continue;
            }
        }
        (void)unlinkat(fd, e->d_name, 0);
    }
    closedir(d);
}

static void procfd_dir_rm(const char *path) {
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (fd >= 0) {
        procfd_dir_empty(fd);
        close(fd);
    }
    (void)rmdir(path);
}

static void procfd_dirs_reap(int force) {
    for (int i = 0; i < 64; i++) {
        if (!g_procfd_dirs[i].path[0]) continue;
        if (force || fcntl(g_procfd_dirs[i].fd, F_GETFD) == -1) {
            procfd_dir_rm(g_procfd_dirs[i].path);
            g_procfd_dirs[i].path[0] = 0;
        }
    }
}

static void procfd_dirs_atexit(void) {
    procfd_dirs_reap(1);
}

static int proc_fd_dir_pid_open(int guest, int host);

// Build the temp dir of fd symlinks and return its fd. The guest fd numbers ARE the host fd numbers here,
// so this process's open fds are exactly the guest's; each link's target is the fd's path (or an
// anon_inode placeholder for a pipe/socket/eventfd with no path). -1 on error.
static int proc_fd_dir_open(void) {
    return proc_fd_dir_pid_open(0, (int)getpid());
}

static void proc_dir_register(int fd, const char *tmpl, const char *guestpath); // defined below (dir synth)

// Build the temp dir of /proc/self/fdinfo entries -- one REGULAR-file placeholder per open fd (content is
// served live by proc_open on the relative reopen). Linux exposes per-fd pos/flags/mnt_id here; runtimes
// read it for descriptor flags, eventfd counters, epoll details. Tagged "/proc/<pid>/fdinfo" so an
// openat(dirfd,"N") re-enters proc_open. Returns the fd, -1 on error.
static int proc_fdinfo_dir_open(const char *guestpath) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-fd-infoXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (eventfd_hidden_peer_fd(fd)) continue;
        if (fcntl(fd, F_GETFD) == -1) continue; // not open
        char p[96];
        snprintf(p, sizeof p, "%s/%d", tmpl, fd);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    int d = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (d < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(d, tmpl, guestpath);
    return d;
}

// The /proc/self/fdinfo/<N> body: Linux reports pos/flags/mnt_id (+ per-type extras). Returns the length or
// -1 if fd N is not open. `off` is the current file offset (lseek CUR), `flags` the O_* access/status bits.
static int proc_fdinfo_text(int fd, char *b, size_t n) {
    if (fd < 0 || fcntl(fd, F_GETFD) == -1) return -1; // not an open fd
    off_t pos = lseek(fd, 0, SEEK_CUR);
    if (pos < 0) pos = 0; // pipe/socket/eventfd: unseekable -> 0, like Linux
    int fl = fcntl(fd, F_GETFL);
    if (fl < 0) fl = 0;
    return snprintf(b, n, "pos:\t%lld\nflags:\t0%o\nmnt_id:\t1\nino:\t1\n", (long long)pos, (unsigned)fl);
}

static int proc_reg_read(int hostpid, char *comm, size_t csz, char *cmd, size_t cmdsz, int *cmdlen);

// The running process's own argv as a NUL-separated, NUL-terminated blob, captured by build_stack at every
// launch/exec. The registry (proc_reg_*) only exists in container/rootfs mode (g_init_hostpid); this global
// makes /proc/self/cmdline reflect the FULL argv even in bare mode -- where a fixed argv[0]-only fallback
// otherwise lost every argument after an exec with many args.
static char g_self_cmdline[8192];
static int g_self_cmdline_len = 0;

static void set_guest_cmdline(int argc, char *const argv[]) {
    int o = 0;
    for (int i = 0; i < argc && argv && argv[i]; i++) {
        int L = (int)strlen(argv[i]);
        if (o + L + 1 > (int)sizeof g_self_cmdline) break;
        memcpy(g_self_cmdline + o, argv[i], (size_t)L);
        o += L;
        g_self_cmdline[o++] = 0;
    }
    g_self_cmdline_len = o;
}

// /proc/[pid]/cmdline -- the guest argv as NUL-separated, NUL-terminated arguments. Prefer the same
// published argv record used for peer /proc/<pid>/cmdline so self-introspection sees process arguments and
// service switches. Fall back to the captured argv blob (bare mode), then argv[0].
static int proc_cmdline_text(char *b, size_t n) {
    char comm[32], cmd[4096];
    int cl;
    if (proc_reg_read((int)getpid(), comm, sizeof comm, cmd, sizeof cmd, &cl) && cl > 0) {
        int L = cl > (int)n ? (int)n : cl;
        memcpy(b, cmd, (size_t)L);
        if (L == 0 || b[L - 1] != 0) {
            if (L < (int)n)
                b[L++] = 0;
            else
                b[L - 1] = 0;
        }
        return L;
    }
    if (g_self_cmdline_len > 0) { // bare mode: the captured argv (all of it, not just argv[0])
        int L = g_self_cmdline_len > (int)n ? (int)n : g_self_cmdline_len;
        memcpy(b, g_self_cmdline, (size_t)L);
        if (b[L - 1] != 0) b[L - 1] = 0;
        return L;
    }
    const char *p = (g_exe_path && g_exe_path[0]) ? g_exe_path : "init";
    int L = (int)strlen(p);
    if (L + 1 > (int)n) L = (int)n - 1;
    memcpy(b, p, (size_t)L);
    b[L] = 0; // cmdline is NUL-terminated (a single empty-tail arg, exactly as the kernel emits)
    return L + 1;
}

// /proc/[pid]/comm -- the task name (Linux comm: basename of the image, max 15 chars) plus a newline.
static int proc_comm_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    return snprintf(b, n, "%s\n", comm);
}

// Append the container's live bind-mount volumes (`-v`/`--mount`/`--tmpfs`) to a mount table. runc lists
// every bind as its own mount line; without them findmnt/df/JVM mount discovery see a namespace that omits
// the guest's binds. `fstab` picks the /proc/mounts (fstab, 6-field) form vs the /proc/self/mountinfo form.
// Single-file binds are skipped so the table shows only
// real directory mount points. Continues from byte `off`; returns the new length (never exceeds `cap-1`).
static size_t mount_binds_append(char *b, size_t cap, size_t off, int fstab) {
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE);
    int id = 100;
    for (int i = 0; i < nv; i++) {
        if (g_vols[i].dead || g_vols[i].isfile) continue;
        if (off + 1 >= cap) break;
        const char *ro = g_vols[i].ro ? "ro" : "rw";
        int w = fstab ? snprintf(b + off, cap - off, "/dev/root %s ext4 %s,relatime 0 0\n", g_vols[i].guest, ro)
                      : snprintf(b + off, cap - off, "%d 23 254:1 / %s %s,relatime - ext4 /dev/root %s\n", id++,
                                 g_vols[i].guest, ro, ro);
        if (w < 0 || (size_t)w >= cap - off) break; // truncated -> stop before overflowing
        off += (size_t)w;
    }
    return off;
}

// /proc/[pid]/mountinfo -- the mounted-filesystem table df/findmnt parse, and which the JVM scans to locate
// the cgroup mount. The rootfs is a single overlay mount at "/"; the pseudo-filesystems (proc, sysfs, the
// cgroup2 hierarchy, devtmpfs) round it out so a reader looking up any of these mount points finds a
// plausible, well-formed line. Field layout: id parent maj:min root mountpoint opts - fstype src superopts.
static int proc_mountinfo_text(char *b, size_t n) {
    // Field layout: id parent maj:min root mountpoint opts - fstype src superopts. The pseudo-mounts and
    // their PARENT ids mirror a real runc/OrbStack container exactly (verified vs the docker oracle): the
    // /dev tmpfs (25) parents /dev/pts, /dev/mqueue and /dev/shm; /sys (28) parents the cgroup2 leaf.
    //  - /sys is READ-ONLY (ro on both the line flags and the sysfs superblock) -- runc binds it ro.
    //  - /dev tmpfs carries size=65536k,mode=755 (docker's default 64M /dev).
    //  - /dev/pts devpts carries gid=5,mode=620,ptmxmode=666 (the devpts mount opts every container shows).
    //  - /dev/shm is its OWN tmpfs mount with src name "shm" (glibc shm_open/DSM back onto it); size=65536k
    //    is docker's default 64M (the host may enlarge it -- size is a host-variant field).
    //  - cgroup2 leaf is ro with src "cgroup" + nsdelegate (JVM/systemd v2 detection keys on this line).
    int len =
        snprintf(b, n,
                 "23 0 0:24 / / rw,relatime - overlay overlay rw\n"
                 "24 23 0:25 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n"
                 "25 23 0:26 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755\n"
                 "26 25 0:27 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,gid=5,mode=620,ptmxmode=666\n"
                 "27 23 0:28 / /sys ro,nosuid,nodev,noexec,relatime - sysfs sysfs ro\n"
                 "28 27 0:29 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw,nsdelegate\n"
                 "29 25 0:30 / /dev/mqueue rw,nosuid,nodev,noexec,relatime - mqueue mqueue rw\n"
                 "30 25 0:31 / /dev/shm rw,nosuid,nodev,noexec,relatime - tmpfs shm rw,size=65536k\n");
    if (len < 0 || (size_t)len >= n) return len;
    return (int)mount_binds_append(b, n, (size_t)len, 0);
}

// /proc/[pid]/mountstats -- the NFS-oriented per-mount statistics file. It fell through to the host, which
// published the entire HOST mount table (block-device names, docker overlay2 hashes, /run/user paths) to
// any guest that read it, while mounts and mountinfo next to it were both intercepted. Only the
// "device X mounted on Y with fstype Z" header lines apply to a container with no NFS mount; derive them
// from the same table mountinfo emits so the three files agree.
static int proc_mountstats_text(char *b, size_t n) {
    char mi[8192];
    int len = proc_mountinfo_text(mi, sizeof mi);
    if (len < 0) return -1;
    int o = 0;
    for (char *line = mi, *end = mi + len; line < end;) {
        char *nl = memchr(line, '\n', (size_t)(end - line));
        if (!nl) break;
        *nl = 0;
        // mountinfo fields: id parent maj:min root MOUNTPOINT opts - FSTYPE SRC superopts.
        char *f[11];
        int nf = 0;
        for (char *tok = strtok(line, " "); tok && nf < 11; tok = strtok(NULL, " "))
            f[nf++] = tok;
        if (nf >= 10) o += snprintf(b + o, n - (size_t)o, "device %s mounted on %s with fstype %s\n", f[8], f[4], f[7]);
        line = nl + 1;
        if ((size_t)o + 128 >= n) break;
    }
    return o;
}

// ================= REAL /proc process table (top/htop/ps) =====================================
// hl's process model: every guest process is its OWN host (macOS) process running this DBT; the
// container init is guest pid 1 (g_init_hostpid<->1), children keep their host pid as the guest pid
// (getpid() returns exactly that). macOS has no /proc, and one DBT process cannot see another's
// address space, so we (1) keep a tiny on-disk REGISTRY where each container process publishes its
// guest identity (comm + full argv), keyed by a per-container tmp dir, and (2) read LIVE per-process
// stats (rss, cpu time, state, ppid) from the host system interface. The union -- registry identity +
// native-process liveness -- lets any process (e.g. `ps`) enumerate the whole container
// and synthesize /proc/<pid>/{stat,status,cmdline,comm} for its peers, with GUEST pids throughout.
#include "../../../host/system.h"

// ABI9 gives every launch an opaque ownership domain independent of networking, hostname, and filesystem
// generation. It is inherited in process memory across every guest fork and survives guest exec. Older
// direct-mode entry points retain the namespace/session fallback until they are removed.
static void proc_reg_key(char *out, size_t n) {
    const char *k = hl_option_get("HL_PROCESS_DOMAIN");
    if (k && strlen(k) == 32) {
        snprintf(out, n, "/tmp/.hl-domain.%s", k);
        return;
    }
    k = hl_option_get("HL_NETNS");
    if (!k || !k[0]) k = hl_option_get("HL_HOSTNAME");
    if (k && k[0]) {
        char s[48];
        int o = 0;
        for (const char *p = k; *p && o < 47; p++)
            if ((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9')) s[o++] = *p;
        s[o] = 0;
        if (o) {
            snprintf(out, n, "/tmp/.hl-pids.%s", s);
            return;
        }
    }
    snprintf(out, n, "/tmp/.hl-pids.s%d", (int)getsid(0));
}

/*
 * One activation may share HL_PROCESS_DOMAIN with other launches in the same
 * container. HL_LAUNCH_DOMAIN is its narrower, activation-owned tree identity.
 * Membership is a birth record only: /proc presentation continues to use the
 * container registry, while activation teardown needs only a PID-reuse-safe
 * list of processes to terminate.
 */
static int launch_reg_key(char *out, size_t n) {
    const char *key = hl_option_get("HL_LAUNCH_DOMAIN");
    size_t index;
    if (!key || strlen(key) != 32) return 0;
    for (index = 0; index < 32; ++index)
        if (!((key[index] >= '0' && key[index] <= '9') || (key[index] >= 'a' && key[index] <= 'f'))) return 0;
    snprintf(out, n, "/tmp/.hl-domain.%s", key);
    return 1;
}

static char g_launch_reg_birth_file[160];

static void launch_reg_publish(int hostpid, int remember) {
    char dir[80], birth[32], path[160];
    hl_host_process_info process;
    if (hostpid <= 0 || !launch_reg_key(dir, sizeof dir) || !hl_host_process_read(hostpid, &process)) return;
    hl_compat_mkdir(dir, 0777);
    int size = snprintf(birth, sizeof birth, "%llu\n", (unsigned long long)process.start_time_ns);
    snprintf(path, sizeof path, "%s/b%d", dir, hostpid);
    if (size > 0 && hl_host_file_store(&g_jit_services, path, 0600, birth, (size_t)size) == 0 && remember)
        snprintf(g_launch_reg_birth_file, sizeof g_launch_reg_birth_file, "%s", path);
}

static void launch_reg_unlink(void) {
    if (!g_launch_reg_birth_file[0]) return;
    (void)hl_host_file_unlink(&g_jit_services, g_launch_reg_birth_file);
    g_launch_reg_birth_file[0] = 0;
}

/* Linux tears down every remaining member of a PID namespace when its init
 * exits.  Each retained-C launch is one such guest domain even though its host
 * processes may escape the initial session with setsid().  Kill only members
 * whose recorded birth identity still matches the live host process, so a
 * recycled host pid can never inherit authority from a stale record.  Repeat
 * until two scans are empty (bounded at two seconds) to close the child-
 * publication race: fork publishes the birth record in the parent before
 * returning to guest code. */
static void launch_reg_terminate_peers(void) {
    char directory[80];
    unsigned empty = 0;
    if (!g_init_hostpid || getpid() != g_init_hostpid || !launch_reg_key(directory, sizeof directory)) return;
    for (unsigned round = 0; round < 200; ++round) {
        unsigned live = 0;
        DIR *entries = opendir(directory);
        if (entries == NULL) return;
        struct dirent *entry;
        while ((entry = readdir(entries)) != NULL) {
            char *end;
            char path[160];
            char text[32];
            long raw;
            uint64_t expected;
            hl_host_process_info process;
            if (entry->d_name[0] != 'b' || entry->d_name[1] < '1' || entry->d_name[1] > '9') continue;
            errno = 0;
            raw = strtol(entry->d_name + 1, &end, 10);
            if (errno != 0 || *end != 0 || raw <= 0 || raw > INT32_MAX || raw == (long)getpid()) continue;
            snprintf(path, sizeof path, "%s/b%ld", directory, raw);
            int descriptor = open(path, O_RDONLY | O_CLOEXEC);
            if (descriptor < 0) {
                (void)unlink(path);
                continue;
            }
            ssize_t count;
            do {
                count = read(descriptor, text, sizeof text - 1);
            } while (count < 0 && errno == EINTR);
            (void)close(descriptor);
            if (count <= 0) {
                (void)unlink(path);
                continue;
            }
            text[count] = 0;
            errno = 0;
            char *birth_end;
            expected = strtoull(text, &birth_end, 10);
            if (errno != 0 || birth_end == text || (*birth_end != '\n' && *birth_end != 0) || expected == 0 ||
                !hl_host_process_read(raw, &process) || process.start_time_ns != expected) {
                (void)unlink(path);
                continue;
            }
            ++live;
            (void)kill((pid_t)raw, SIGKILL);
            (void)unlink(path);
        }
        (void)closedir(entries);
        if (live == 0) {
            if (++empty == 2) {
                (void)rmdir(directory);
                return;
            }
        } else {
            empty = 0;
        }
        (void)poll(NULL, 0, 10);
    }
}

// This process's own registry file (unlinked on exit; the exit_group path calls proc_reg_unlink since
// _exit bypasses atexit). Stale files from a crash are pruned lazily by the enumerator (dead-pid check).
static char g_reg_file[128];
static char g_reg_exe_file[128];   // sibling "x<pid>" record: the canonical exe path (for /proc/<pid>/exe)
static char g_reg_birth_file[160]; // sibling "b<pid>": native start time, preventing PID-reuse kills
static char g_reg_last_buf[4096];
static int g_reg_last_len;
static char g_reg_last_exe[4200];

static void proc_reg_unlink(void) {
    launch_reg_unlink();
    if (g_reg_file[0]) {
        (void)hl_host_file_unlink(&g_jit_services, g_reg_file);
        g_reg_file[0] = 0;
    }
    if (g_reg_exe_file[0]) {
        (void)hl_host_file_unlink(&g_jit_services, g_reg_exe_file);
        g_reg_exe_file[0] = 0;
    }
    if (g_reg_birth_file[0]) {
        (void)hl_host_file_unlink(&g_jit_services, g_reg_birth_file);
        g_reg_birth_file[0] = 0;
    }
}

static void proc_reg_write_files(const char *dir, const char *buf, int len, const char *exe) {
    char tmp[144];
    snprintf(tmp, sizeof tmp, "%s/.t%d", dir, (int)getpid());
    if (hl_host_file_store(&g_jit_services, tmp, 0644, buf, (size_t)len) != 0) return;
    char final[128];
    snprintf(final, sizeof final, "%s/%d", dir, (int)getpid());
    if (hl_host_file_rename(&g_jit_services, tmp, final) == 0)
        snprintf(g_reg_file, sizeof g_reg_file, "%s", final);
    else
        (void)hl_host_file_unlink(&g_jit_services, tmp);
    // Publish the CANONICAL exe path as a sibling "x<pid>" record so a PEER process can serve
    // readlink("/proc/<pid>/exe") for this one (`ls -l /proc/<pid>`, ps tooling). The non-digit-leading
    // name keeps it invisible to the pid enumerators (proc_reg_count / the /proc listing digit scan).
    if (exe && exe[0] == '/') {
        char xtmp[152], xfin[144];
        snprintf(xtmp, sizeof xtmp, "%s/.xt%d", dir, (int)getpid());
        snprintf(xfin, sizeof xfin, "%s/x%d", dir, (int)getpid());
        if (hl_host_file_store(&g_jit_services, xtmp, 0644, exe, strlen(exe)) == 0) {
            if (hl_host_file_rename(&g_jit_services, xtmp, xfin) == 0) {
                if (path_copy(g_reg_exe_file, sizeof g_reg_exe_file, xfin) != 0)
                    (void)hl_host_file_unlink(&g_jit_services, xfin);
            } else
                (void)hl_host_file_unlink(&g_jit_services, xtmp);
        }
    }
    {
        hl_host_process_info process;
        char birth[32], path[144];
        if (hl_host_process_read(getpid(), &process)) {
            int size = snprintf(birth, sizeof birth, "%llu\n", (unsigned long long)process.start_time_ns);
            snprintf(path, sizeof path, "%s/b%d", dir, (int)getpid());
            if (size > 0 && hl_host_file_store(&g_jit_services, path, 0600, birth, (size_t)size) == 0)
                snprintf(g_reg_birth_file, sizeof g_reg_birth_file, "%s", path);
        }
    }
}

// Publish THIS process's guest identity: "<comm>\n" then the full argv NUL-separated. Written to a temp
// name + renamed for an atomic publish. Called at startup and after each guest execve (comm changes).
static void proc_reg_publish(const char *exe, int argc, char *const argv[]) {
    launch_reg_publish((int)getpid(), 1);
    if (!g_init_hostpid) return; // process table is a container feature
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    hl_compat_mkdir(dir, 0777);
    static int reg = 0;
    if (!reg) {
        atexit(proc_reg_unlink);
        reg = 1;
    }
    char comm[16];
    proc_comm(comm, sizeof comm); // the recorded exec-name (set_guest_comm), NOT basename(exe): a script
                                  // exec keeps the script's name even though `exe` is the interpreter
    char buf[4096];
    int o = snprintf(buf, sizeof buf, "%s\n", comm), wrote = 0;
    if (o < 0) return;
    if (o >= (int)sizeof buf) o = (int)sizeof buf - 1;
    if (argv)
        for (int i = 0; i < argc && argv[i] && o < (int)sizeof buf - 1; i++) {
            int L = (int)strlen(argv[i]);
            if (o + L + 1 > (int)sizeof buf) break;
            memcpy(buf + o, argv[i], (size_t)L);
            o += L;
            buf[o++] = 0;
            wrote = 1;
        }
    if (!wrote) { // no argv retained -> the exe path is the single cmdline arg (matches proc_cmdline_text)
        const char *e = (exe && exe[0]) ? exe : "init";
        int L = (int)strlen(e);
        if (o + L + 1 <= (int)sizeof buf) {
            memcpy(buf + o, e, (size_t)L);
            o += L;
            buf[o++] = 0;
        }
    }
    memcpy(g_reg_last_buf, buf, (size_t)o);
    g_reg_last_len = o;
    if (exe && exe[0])
        snprintf(g_reg_last_exe, sizeof g_reg_last_exe, "%s", exe);
    else
        g_reg_last_exe[0] = 0;
    proc_reg_write_files(dir, buf, o, g_reg_last_exe);
}

// Publish a task-name change without replacing the argv snapshot. prctl(PR_SET_NAME) and a write to
// /proc/self/comm change only comm; peers must observe that change through the process registry too.
static void proc_reg_publish_comm(void) {
    if (!g_init_hostpid || g_reg_last_len <= 0) return;
    char comm[16];
    proc_comm(comm, sizeof comm);
    char *newline = memchr(g_reg_last_buf, '\n', (size_t)g_reg_last_len);
    int tail = newline ? g_reg_last_len - (int)(newline - g_reg_last_buf) - 1 : 0;
    char updated[sizeof g_reg_last_buf];
    int head = snprintf(updated, sizeof updated, "%s\n", comm);
    if (head < 0 || head + tail > (int)sizeof updated) return;
    if (tail > 0) memcpy(updated + head, newline + 1, (size_t)tail);
    memcpy(g_reg_last_buf, updated, (size_t)(head + tail));
    g_reg_last_len = head + tail;
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    proc_reg_write_files(dir, g_reg_last_buf, g_reg_last_len, g_reg_last_exe);
}

static void proc_reg_after_fork(void) {
    g_launch_reg_birth_file[0] = 0;
    launch_reg_publish((int)getpid(), 1);
    if (!g_init_hostpid) return;
    // A fork child inherits the parent's g_reg_file paths. Clear them before publishing, otherwise the
    // child's exit_group cleanup can unlink the parent's /proc registry entry.
    g_reg_file[0] = 0;
    g_reg_exe_file[0] = 0;
    g_reg_birth_file[0] = 0;
    if (g_reg_last_len <= 0) {
        char *argv[] = {(char *)g_exe_path, NULL};
        proc_reg_publish(g_exe_path, 1, argv);
        return;
    }
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    hl_compat_mkdir(dir, 0777);
    proc_reg_write_files(dir, g_reg_last_buf, g_reg_last_len, g_reg_last_exe);
}

// Read a peer's published canonical exe path (the "x<hostpid>" registry record). Returns 1 + fills out.
static int proc_reg_exe_read(int hostpid, char *out, size_t n) {
    char dir[80], path[144];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/x%d", dir, hostpid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    char buf[4200];
    ssize_t nr = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (nr <= 0) return 0;
    buf[nr] = 0;
    if (buf[0] != '/') return 0;
    snprintf(out, n, "%s", buf);
    return 1;
}

// /proc/<peer>/maps for another process in the same container. hl cannot inspect a peer engine process's
// guest VMA registry from here, but Linux software is allowed to open this file and expects structured maps
// text rather than ENOENT. Publish a conservative non-empty shape using the peer's registered exe path plus
// plausible heap/stack rows; self reads still use the exact gmap-backed proc_maps_fd() above.
static int proc_maps_pid_fd(int gp, int host) {
    (void)gp;
    char exe[4200];
    if (!proc_reg_exe_read(host, exe, sizeof exe)) snprintf(exe, sizeof exe, "/proc/%d/exe", host);

    char buf[24576]; // 5 rows, each able to carry the full 4 KB exe path without being truncated mid-row
    int n = 0;
    // The peer's image rows carry its own dev:inode when the path is stattable, so a reader that keys on the
    // pair (rather than the pathname) classifies them as file-backed exactly as it would on Linux.
    unsigned dmaj = 0, dmin = 0;
    unsigned long long ino = 0;
    struct stat es;
    if (stat(exe, &es) == 0) {
        dmaj = hl_linux_device_major((uint64_t)es.st_dev);
        dmin = hl_linux_device_minor((uint64_t)es.st_dev);
        ino = (unsigned long long)es.st_ino;
    }
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x400000, 0x500000, "r-xp", 0, dmaj, dmin, ino, exe, 0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x500000, 0x510000, "r--p", 0x100000, dmaj, dmin, ino, exe,
                           0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x510000, 0x520000, "rw-p", 0x110000, dmaj, dmin, ino, exe,
                           0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x70000000, 0x70100000, "rw-p", 0, 0, 0, 0, "[heap]", 0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x7ffde000, 0x7ffff000, "rw-p", 0, 0, 0, 0, "[stack]", 0);
    char desc[64];
    snprintf(desc, sizeof desc, "pid:%d:maps", gp);
    return proc_text_fd_tagged(buf, n, desc);
}

// Read back a peer's published identity by host pid. Returns 1 + fills comm and the NUL-separated
// cmdline (cmdlen bytes); 0 if no record. The comm line is stripped from the returned cmdline.
static int proc_reg_read(int hostpid, char *comm, size_t csz, char *cmd, size_t cmdsz, int *cmdlen) {
    char dir[80], path[128];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, hostpid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    char buf[4096];
    int nr = (int)read(fd, buf, sizeof buf - 1);
    close(fd);
    if (nr <= 0) return 0;
    buf[nr] = 0;
    char *nl = memchr(buf, '\n', (size_t)nr);
    int cl = nl ? (int)(nl - buf) : 0;
    if (cl >= (int)csz) cl = (int)csz - 1;
    memcpy(comm, buf, (size_t)cl);
    comm[cl] = 0;
    int off = nl ? (int)(nl - buf + 1) : nr, rem = nr - off;
    if (rem < 0) rem = 0;
    if (rem > (int)cmdsz) rem = (int)cmdsz;
    memcpy(cmd, buf + off, (size_t)rem);
    *cmdlen = rem;
    return 1;
}

// Live per-process stats from the host backend. rss/cpu-times/state are REAL (coarse beats
// zero); comm here is the HOST comm (the DBT binary) -- the guest comm comes from the registry instead.
struct hl_procinfo {
    int ppid_host, pgid_host, nthreads;
    char state;
    unsigned long long rss, vsize, utime_ns, stime_ns;
    long start_sec;
    char hostcomm[32];
};

static int hl_get_procinfo(int pid, struct hl_procinfo *pi) {
    hl_host_process_info host;
    if (!hl_host_process_read(pid, &host)) return 0;
    pi->ppid_host = (int)host.parent_pid;
    pi->pgid_host = (int)host.process_group;
    pi->start_sec = (long)host.start_time_seconds;
    pi->state = host.state;
    pi->rss = host.resident_bytes;
    pi->vsize = host.virtual_bytes;
    pi->utime_ns = host.user_time_ns;
    pi->stime_ns = host.system_time_ns;
    pi->nthreads = host.threads > 0 ? (int)host.threads : 1;
    snprintf(pi->hostcomm, sizeof pi->hostcomm, "%s", host.name);
    return 1;
}

// Rebase a host vnode path into the container's guest namespace (strip the rootfs prefix), in place.
static int proc_fd_rebase(char *tgt, size_t capacity) {
    char guest[4200];
    /* guest_from_host is the single longest-prefix authority for the writable root, every read-only
     * image layer, and nested volumes.  Repeating only the root/volume subset here made descriptors
     * opened directly from a lower layer (for example /tmp) look external to the container. */
    int status = guest_from_host(tgt, guest, sizeof guest);
    if (status > 0) {
        if (path_copy(tgt, capacity, guest) == 0) return 1;
        if (capacity != 0) tgt[0] = 0;
        return -ENAMETOOLONG;
    }
    return status;
}

static int proc_fdvis_resolve_host(int host, int guest_fd) {
    uint32_t kind;
    uint64_t device, object;
    size_t count = 0;
    if (!proc_fdvis_lookup(host, guest_fd, &kind, &device, &object)) return guest_fd;
    if (device == 0 || object == 0 || !hl_host_process_fds(host, NULL, 0, &count)) return -1;
    hl_host_process_fd *entries = count ? malloc(count * sizeof *entries) : NULL;
    if (count && !entries) return -1;
    if (!hl_host_process_fds(host, entries, count, &count)) {
        free(entries);
        return -1;
    }
    int resolved = -1;
    for (size_t index = 0; index < count; ++index) {
        hl_host_process_fd detail;
        size_t ignored;
        if (hl_host_process_fd_read(host, entries[index].descriptor, &detail, NULL, 0, &ignored) &&
            detail.stable_device == device && detail.stable_object == object &&
            (kind == HL_HOST_FD_OTHER || detail.kind == kind)) {
            resolved = entries[index].descriptor;
            break;
        }
    }
    free(entries);
    return resolved;
}

// The /proc/<pid>/fd/<fd> readlink target for a PEER container process (host pid `host`), the SYMLINK-TARGET
// view. A guest process is its own macOS process with a PRIVATE fd table, so the peer's fds aren't in our
// own table (procfd_num rejects a foreign pid) -- read them through host process inspection: a file's
// native path (rebased out of the rootfs), a pipe/socket/anon fd as the Linux-style
// "pipe:[..]"/"socket:[..]"/"anon_inode:[..]" name. Returns the byte length written to `out`, or -1 if the
// peer or fd is not resolvable (-> ENOENT). Guest fd numbers == host fd numbers, the same 1:1 mapping the
// self /proc/self/fd view relies on.
static int proc_fd_link_pid(int host, int fd, char *out, size_t n) {
    hl_host_process_fd entry;
    char tgt[4200] = {0};
    size_t target_size = 0;
    int inspected_fd;
    if (host <= 0 || fd < 0) return -1;
    uint32_t logical_kind = HL_HOST_FD_OTHER;
    uint64_t logical_device = 0, logical_object = 0;
    int logical_found = proc_fdvis_lookup(host, fd, &logical_kind, &logical_device, &logical_object);
    if (logical_found && logical_kind != HL_HOST_FD_FILE && logical_object != 0) {
        const char *logical_name = logical_kind == HL_HOST_FD_SOCKET ? "socket"
                                   : logical_kind == HL_HOST_FD_PIPE ? "pipe"
                                                                     : "anon_inode";
        char logical[64];
        int length = snprintf(logical, sizeof logical, "%s:[%llu]", logical_name, (unsigned long long)logical_object);
        if ((size_t)length > n) length = (int)n;
        memcpy(out, logical, (size_t)length);
        return length;
    }
    /* A provider-backed descriptor has no reliable native descriptor in this
     * process's fd table -- resolving it by device/object identity can collide
     * with an unrelated engine-private fd. The engine's fd->path table is
     * authoritative for every tracked self descriptor, including directories. */
    if (host == (int)getpid() && logical_found && fd >= 0 && fd < HL_NFD && g_fdpath[fd][0]) {
        char tracked[4200];
        snprintf(tracked, sizeof tracked, "%s", g_fdpath[fd]);
        int mapped = g_fdpath_guest[fd] ? 1 : proc_fd_rebase(tracked, sizeof tracked);
        if (mapped < 0 || (g_rootfs && mapped == 0)) return -1;
        size_t l = strlen(tracked);
        if (l > n) l = n;
        memcpy(out, tracked, l);
        return (int)l;
    }
    inspected_fd = proc_fdvis_resolve_host(host, fd);
    if (inspected_fd < 0) return -1;
    if (!hl_host_process_fd_read(host, inspected_fd, &entry, tgt, sizeof tgt - 1, &target_size)) return -1;
    if (entry.kind == HL_HOST_FD_FILE && target_size != 0) {
        tgt[target_size] = 0;
        /* A launch-scoped controlling terminal is the first slave in the
         * guest devpts namespace regardless of the host's global pty number.
         * Only typed launch stdio receives this projection; ordinary host
         * binds and guest-created ptys retain their own namespace identity. */
        int projected_tty = logical_found && fd >= 0 && fd <= STDERR_FILENO &&
                            (strncmp(tgt, "/dev/pts/", 9) == 0 || strncmp(tgt, "/dev/ttys", 9) == 0);
        if (projected_tty) snprintf(tgt, sizeof tgt, "/dev/pts/0");
        int mapped = projected_tty ? 1 : proc_fd_rebase(tgt, sizeof tgt);
        if (mapped < 0 || (g_rootfs && mapped == 0)) return -1;
        size_t l = strlen(tgt);
        if (l > n) l = n;
        memcpy(out, tgt, l);
        return (int)l;
    }
    const char *k = entry.kind == HL_HOST_FD_SOCKET ? "socket" : entry.kind == HL_HOST_FD_PIPE ? "pipe" : "anon_inode";
    char syn[64];
    int sl = snprintf(syn, sizeof syn, "%s:[%d]", k, fd);
    if ((size_t)sl > n) sl = (int)n;
    memcpy(out, syn, (size_t)sl);
    return sl;
}

// Is `fd` currently OPEN in the PEER process `host`? (For peer /proc/<pid>/fd/<N> lstat/stat: a live fd is a
// symlink, a closed one ENOENTs.) Returns 1 if open, 0 otherwise.
static int proc_fd_pid_open_one(int host, int fd) {
    hl_host_process_fd entry;
    size_t path_size;
    int inspected_fd;
    if (host <= 0 || fd < 0) return 0;
    inspected_fd = proc_fdvis_resolve_host(host, fd);
    if (inspected_fd < 0) return 0;
    return hl_host_process_fd_read(host, inspected_fd, &entry, NULL, 0, &path_size);
}

// Build a temp dir of "N -> target" symlinks for a PEER container process's open fds (host pid `host`), so
// a peer /proc/<pid>/fd is listable (getdents) and each entry readlinks to the fd's target -- the same
// symlink-dir mechanism proc_fd_dir_open() uses for self, but populated from the peer descriptor snapshot
// instead of our own host fd table. Self is delegated to proc_fd_dir_open (exact host table). Returns the
// dir fd, or -1. NOTE: this is the LISTING + readlink view only; actually OPENING a peer fd (using
// /proc/<pid>/fd/N as a working descriptor) needs the owner to hand the real fd across processes
// (SCM_RIGHTS-level fd passing) -- deferred; open of a peer fd link still ENOENTs.
static int proc_fd_dir_pid_open(int guest, int host) {
    int self = guest == 0;
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    size_t nfd = 0;
    if (!hl_host_process_fds(host, NULL, 0, &nfd)) return -1;
    size_t fd_capacity = nfd;
    hl_host_process_fd *fds = fd_capacity != 0 ? malloc(fd_capacity * sizeof *fds) : NULL;
    if (fd_capacity != 0 && !fds) return -1;
    if (!hl_host_process_fds(host, fds, fd_capacity, &nfd)) {
        free(fds);
        return -1;
    }
    if (nfd > fd_capacity) nfd = fd_capacity;
    int identity = self ? host : guest;
    size_t nviews = proc_fdvis_list(identity, NULL, 0);
    struct fdvis_view *views = nviews ? malloc(nviews * sizeof *views) : NULL;
    if (nviews && !views) {
        free(fds);
        return -1;
    }
    if (nviews) {
        size_t copied = proc_fdvis_list(identity, views, nviews);
        if (copied < nviews) nviews = copied;
    }
    char tmpl[] = "/tmp/.hl-proc-fd-dirXXXXXX";
    if (!mkdtemp(tmpl)) {
        free(views);
        free(fds);
        return -1;
    }
    for (size_t i = 0; i < nfd; i++) {
        int fd = fds[i].descriptor;
        char tgt[4200] = {0};
        size_t target_size = 0;
        hl_host_process_fd entry = {.descriptor = -1};
        int have = hl_host_process_fd_read(host, fd, &entry, tgt, sizeof tgt - 1, &target_size) &&
                   entry.kind == HL_HOST_FD_FILE && target_size != 0;
        int hidden = nviews != 0 || (fds[i].flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0;
        for (size_t view = 0; view < nviews && !hidden; ++view)
            if (views[view].guest_fd == fd) hidden = 1;
        if (!hidden && have && strstr(tgt, "/.hl-proc-fd-dir") != NULL)
            for (size_t view = 0; view < nviews && !hidden; ++view)
                if (entry.stable_device != 0 && entry.stable_object != 0 && views[view].device == entry.stable_device &&
                    views[view].object == entry.stable_object)
                    hidden = 1;
        if (hidden) continue;
        if (entry.descriptor == fd) fds[i].kind = entry.kind;
        if (have) {
            tgt[target_size] = 0;
            int mapped = proc_fd_rebase(tgt, sizeof tgt);
            have = mapped >= 0 && (!g_rootfs || mapped > 0) && tgt[0] != 0;
        }
        if (!have) {
            const char *k = fds[i].kind == HL_HOST_FD_SOCKET ? "socket"
                            : fds[i].kind == HL_HOST_FD_PIPE ? "pipe"
                                                             : "anon_inode";
            snprintf(tgt, sizeof tgt, "%s:[%d]", k, fd);
        }
        char link[80];
        snprintf(link, sizeof link, "%s/%d", tmpl, fd);
        if (symlink(tgt, link) != 0) {}
    }
    for (size_t view = 0; view < nviews; ++view) {
        char tgt[4200] = {0};
        int length = proc_fd_link_pid(identity, views[view].guest_fd, tgt, sizeof tgt - 1);
        if (length <= 0) continue;
        tgt[length] = 0;
        char link[80];
        snprintf(link, sizeof link, "%s/%d", tmpl, views[view].guest_fd);
        if (symlink(tgt, link) != 0) {}
    }
    free(views);
    free(fds);
    int d = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (d < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    if (self) {
        struct stat status;
        char link[80];
        char target[64];
        snprintf(link, sizeof link, "%s/%d", tmpl, d);
        snprintf(target, sizeof target, "/proc/self/fd/%d", d);
        if (symlink(target, link) != 0 && errno != EEXIST) {}
        if (fstat(d, &status) == 0) {
            /* This directory is returned to the guest and therefore is not engine-private. Publish its
             * logical identity normally; private adoption would move it outside the guest fd range. */
            if (proc_fdvis_publish(d, HL_HOST_FD_FILE, (uint64_t)status.st_dev, (uint64_t)status.st_ino) != 0) {
                close(d);
                procfd_dir_rm(tmpl);
                return -1;
            }
        }
    }
    if (self) {
        /* Tag the materialized directory with its guest namespace path. Relative openat/stat/readlink
         * operations must re-enter procfd synthesis instead of following the temporary host symlinks. */
        proc_dir_register(d, tmpl, "/proc/self/fd");
    } else {
        for (int i = 0; i < 64; i++)
            if (!g_procfd_dirs[i].path[0]) {
                g_procfd_dirs[i].fd = d;
                snprintf(g_procfd_dirs[i].path, sizeof g_procfd_dirs[i].path, "%s", tmpl);
                break;
            }
    }
    return d;
}

// Resident footprint (bytes) for OUR OWN pid's VmRSS / statm-resident / stat-rss. The guest's tracked anon
// charge (g_mem_charged) is 0 for a process that has only faulted its static image, but a real Linux process
// ALWAYS has a non-zero VmRSS -- top/htop/ps would otherwise show this process at RES=0, a engine-specific divergence
// (a peer pid already reports a live resident size through host process stats; self must not read 0). Floor the tracked
// charge with this engine process's real resident size so the reported RSS is non-zero and plausible.
