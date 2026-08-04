// Broad directory/file-mutation errno matrix: rmdir, rename/renameat2, mkdir, symlink/link,
// truncate/chmod/chown, access. Shells, package managers and libc branch on these exact errnos
// (EISDIR vs EPERM, ENOTEMPTY vs EEXIST, missing ENOTDIR), so every failure mode must match Linux.
// Boolean-per-op so the golden is uid-robust; runs in the bare scratch dir (non-bound path).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1 << 0)
#endif
#ifndef RENAME_EXCHANGE
#define RENAME_EXCHANGE (1 << 1)
#endif

static char B[128];
static char p1[512], p2[512];
static char *P(char *buf, const char *rel) { snprintf(buf, 512, "%s/%s", B, rel); return buf; }
// returns 1 iff op failed with the expected errno.
static int fail_is(int rc, int want) { return rc != 0 && errno == want; }

int main(void) {
    snprintf(B, sizeof B, "hl_errmat_%d", (int)getpid());
    mkdir(B, 0755);

    // rmdir edge errnos.
    int rmdir_dot_einval   = fail_is(rmdir(P(p1, ".")), EINVAL);
    int rmdir_dotdot_ne    = fail_is(rmdir(P(p1, "..")), ENOTEMPTY) || fail_is(rmdir(p1), EINVAL);
    mkdir(P(p1, "ne"), 0755); close(open(P(p2, "ne/x"), O_CREAT | O_WRONLY, 0644));
    int rmdir_nonempty     = fail_is(rmdir(P(p1, "ne")), ENOTEMPTY);
    int rmdir_onfile       = fail_is(rmdir(P(p1, "ne/x")), ENOTDIR);
    int rmdir_missing      = fail_is(rmdir(P(p1, "gone")), ENOENT);
    unlink(P(p2, "ne/x")); rmdir(P(p1, "ne"));

    // rename / renameat2 edge errnos.
    mkdir(P(p1, "da"), 0755); mkdir(P(p2, "db"), 0755);
    close(open(P(p1, "da/inner"), O_CREAT | O_WRONLY, 0644));
    int ren_dir_over_nonempty = fail_is(rename(P(p1, "db"), P(p2, "da")), ENOTEMPTY);
    close(open(P(p1, "fileA"), O_CREAT | O_WRONLY, 0644));
    int ren_file_over_dir  = fail_is(rename(P(p1, "fileA"), P(p2, "da")), EISDIR);
    int ren_dir_over_file  = fail_is(rename(P(p1, "db"), P(p2, "fileA")), ENOTDIR);
    int ren_missing_src    = fail_is(rename(P(p1, "nope"), P(p2, "fileB")), ENOENT);
    int ren_self_noop      = rename(P(p1, "fileA"), P(p2, "fileA")) == 0;
    int ren_subdir_of_self = fail_is(rename(P(p1, "da"), P(p2, "da/sub")), EINVAL);
    snprintf(p1, sizeof p1, "%s/fileA/", B);
    int ren_trailslash     = fail_is(rename(p1, P(p2, "fileC")), ENOTDIR);
    close(open(P(p1, "rn_a"), O_CREAT | O_WRONLY, 0644));
    close(open(P(p2, "rn_b"), O_CREAT | O_WRONLY, 0644));
    int r2_noreplace = fail_is(syscall(SYS_renameat2, AT_FDCWD, P(p1, "rn_a"), AT_FDCWD,
                                       P(p2, "rn_b"), RENAME_NOREPLACE), EEXIST);
    int r2_bothflags = fail_is(syscall(SYS_renameat2, AT_FDCWD, P(p1, "rn_a"), AT_FDCWD,
                                       P(p2, "rn_b"), RENAME_NOREPLACE | RENAME_EXCHANGE), EINVAL);
    int r2_exch_miss = fail_is(syscall(SYS_renameat2, AT_FDCWD, P(p1, "rn_a"), AT_FDCWD,
                                       P(p2, "rn_gone"), RENAME_EXCHANGE), ENOENT);

    // mkdir edge errnos.
    int mkdir_existing     = fail_is(mkdir(P(p1, "da"), 0755), EEXIST);
    int mkdir_under_file   = fail_is(mkdir(P(p1, "fileA/x"), 0755), ENOTDIR);
    int mkdir_under_missing = fail_is(mkdir(P(p1, "noexist/x"), 0755), ENOENT);

    // symlink / link edge errnos.
    int symlink_over_exist = fail_is(symlink("t", P(p1, "fileA")), EEXIST);
    char longtgt[5000]; memset(longtgt, 'a', sizeof longtgt - 1); longtgt[sizeof longtgt - 1] = 0;
    int symlink_toolong    = fail_is(symlink(longtgt, P(p1, "lnk_long")), ENAMETOOLONG);
    int link_missing_src   = fail_is(link(P(p1, "nope"), P(p2, "lk1")), ENOENT);
    int link_dir_eperm     = fail_is(link(P(p1, "da"), P(p2, "lk_dir")), EPERM);
    close(open(P(p1, "lk_src"), O_CREAT | O_WRONLY, 0644));
    int link_over_exist    = fail_is(link(P(p1, "lk_src"), P(p2, "fileA")), EEXIST);

    // truncate / chmod / chown edge errnos.
    int trunc_dir_eisdir   = fail_is(truncate(P(p1, "da"), 0), EISDIR);
    int trunc_missing      = fail_is(truncate(P(p1, "nope"), 0), ENOENT);
    int trunc_negative     = fail_is(truncate(P(p1, "lk_src"), -1), EINVAL);
    int chmod_missing      = fail_is(chmod(P(p1, "nope"), 0644), ENOENT);
    int chown_missing      = fail_is(chown(P(p1, "nope"), 0, 0), ENOENT);

    // access X_OK on a non-exec file: EACCES for a non-root uid; root's DAC override makes X_OK succeed.
    close(open(P(p1, "noexec"), O_CREAT | O_WRONLY, 0600));
    int access_xok = getuid() == 0 ? (access(P(p1, "noexec"), X_OK) == 0)
                                   : fail_is(access(P(p1, "noexec"), X_OK), EACCES);

    printf("errno-matrix rmdir=%d rename=%d renameat2=%d mkdir=%d linksym=%d trunc=%d access=%d\n",
           rmdir_dot_einval && rmdir_dotdot_ne && rmdir_nonempty && rmdir_onfile && rmdir_missing,
           ren_dir_over_nonempty && ren_file_over_dir && ren_dir_over_file && ren_missing_src &&
               ren_self_noop && ren_subdir_of_self && ren_trailslash,
           r2_noreplace && r2_bothflags && r2_exch_miss,
           mkdir_existing && mkdir_under_file && mkdir_under_missing,
           symlink_over_exist && symlink_toolong && link_missing_src && link_dir_eperm && link_over_exist,
           trunc_dir_eisdir && trunc_missing && trunc_negative && chmod_missing && chown_missing,
           access_xok);
    return 0;
}
