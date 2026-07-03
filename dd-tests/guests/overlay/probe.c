// Overlay-correctness probe. Run by dd-tests/tests/overlay.rs with a SYNTHETIC read-only lower layer
// (an "image") + an empty writable upper (the container rootfs). It exercises the union/whiteout/
// copy-up/opaque/xattr semantics that real Linux overlayfs guarantees and prints one `KEY=VALUE` line
// per observation; the Rust harness asserts the VALUEs against real-overlayfs ground truth. Every check
// is deterministic and self-contained (no clock/network). Built static-PIE for both Linux guest arches.
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/xattr.h>
#include <unistd.h>

static int rd(const char *p, char *buf, int n) {
    int fd = open(p, O_RDONLY);
    if (fd < 0) return -1;
    int k = (int)read(fd, buf, n - 1);
    close(fd);
    if (k < 0) return -1;
    buf[k] = 0;
    return k;
}

static int renameat2_(const char *o, const char *n, unsigned flags) {
    return (int)syscall(SYS_renameat2, AT_FDCWD, o, AT_FDCWD, n, flags);
}

// sorted, comma-joined non-dot directory listing (simple insertion sort, small dirs)
static void listdir(const char *p, char *out, int n) {
    char names[64][256];
    int c = 0;
    DIR *d = opendir(p);
    out[0] = 0;
    if (!d) { snprintf(out, n, "<ENOENT>"); return; }
    struct dirent *e;
    while ((e = readdir(d)) && c < 64) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        snprintf(names[c++], 256, "%s", e->d_name);
    }
    closedir(d);
    for (int i = 0; i < c; i++)
        for (int j = i + 1; j < c; j++)
            if (strcmp(names[j], names[i]) < 0) {
                char t[256];
                memcpy(t, names[i], 256);
                memcpy(names[i], names[j], 256);
                memcpy(names[j], t, 256);
            }
    int off = 0;
    for (int i = 0; i < c; i++)
        off += snprintf(out + off, n - off, "%s%s", i ? "," : "", names[i]);
    if (!c) snprintf(out, n, "<empty>");
}

int main(void) {
    char buf[512];

    // ---- G2: rename of a lower-only POPULATED directory must preserve its whole subtree ----
    // Real overlayfs: `mv` succeeds and every child (incl. nested) moves to the destination; the source
    // disappears. The dd bug materialised the lower dir as an EMPTY upper and moved that -> data loss.
    int rr = rename("/g2src", "/g2dst");
    printf("g2_rename_ret=%d\n", rr < 0 ? -1 : 0);
    printf("g2_dst_f1=%s\n", rd("/g2dst/f1", buf, sizeof buf) >= 0 ? buf : "<none>");
    printf("g2_dst_f2=%s\n", rd("/g2dst/f2", buf, sizeof buf) >= 0 ? buf : "<none>");
    printf("g2_dst_nested=%s\n", rd("/g2dst/sub/nested", buf, sizeof buf) >= 0 ? buf : "<none>");
    printf("g2_src_gone=%d\n", access("/g2src", F_OK) < 0 ? 1 : 0);

    // ---- G3: copy-up must PRESERVE mode (incl. setuid) and mtime ----
    // A no-op chown triggers copy-up without changing content; the upper copy must keep the lower's
    // 04755 mode and its original mtime (1000000000), not 0755/now.
    chown("/meta", (uid_t)-1, (gid_t)-1);
    struct stat st;
    if (stat("/meta", &st) == 0) {
        printf("g3_mode=%o\n", st.st_mode & 07777);
        printf("g3_mtime=%ld\n", (long)st.st_mtime);
    } else {
        printf("g3_mode=stat-failed\n");
        printf("g3_mtime=stat-failed\n");
    }

    // ---- G5: xattr passthrough. setxattr/getxattr/listxattr must actually round-trip (stubbed to
    // ignore/ENODATA today, a correctness trap for file-caps/security images). setxattr on a lower-only
    // file must copy it up first (and preserve its bytes), then persist the attr. ----
    int fd = open("/xf2", O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) close(fd);
    int sx = (int)setxattr("/xf2", "user.a", "hello", 5, 0);
    long gx2 = getxattr("/xf2", "user.a", buf, sizeof buf);
    printf("g5_set_ret=%d\n", sx < 0 ? -1 : 0);
    printf("g5_get=%s\n", gx2 > 0 ? (buf[gx2] = 0, buf) : "<none>");
    long lx = listxattr("/xf2", buf, sizeof buf);
    int has_a = 0;
    for (long i = 0; i < lx && lx <= (long)sizeof buf;) {
        if (!strcmp(buf + i, "user.a")) has_a = 1;
        i += strlen(buf + i) + 1;
    }
    printf("g5_list_has_user_a=%d\n", has_a);
    // setxattr on a lower-only file copies it up; bytes preserved AND attr readable afterwards.
    setxattr("/xf", "user.k", "v1", 2, 0);
    long gxl = getxattr("/xf", "user.k", buf, sizeof buf);
    char kept[64];
    printf("g5_copyup_get=%s\n", gxl > 0 ? (buf[gxl] = 0, buf) : "<none>");
    printf("g5_copyup_bytes=%s\n", rd("/xf", kept, sizeof kept) >= 0 ? kept : "<none>");

    // ---- G6 + G1(runtime): remove a lower-backed dir then recreate it; lower children must NOT leak ----
    // `rm -rf /opqdir && mkdir /opqdir` -> the new dir is opaque: readdir shows only what we put in it,
    // never the lower's stale1/stale2. A stale .wh. marker must also not hide the freshly-made dir.
    unlink("/opqdir/stale1");
    unlink("/opqdir/stale2");
    rmdir("/opqdir");
    // After `rm -rf`, a lower-backed dir must be truly GONE (real overlayfs drops a whiteout and removes
    // the whole upper copy). The dd bug left a non-empty upper dir behind (rmdir couldn't remove it while
    // it still held child `.wh.` markers), so `/opqdir` wrongly still resolved as existing.
    printf("g1r_gone_after_rm=%d\n", access("/opqdir", F_OK) < 0 ? 1 : 0);
    mkdir("/opqdir", 0755);
    printf("g6_dir_visible=%d\n", access("/opqdir", F_OK) == 0 ? 1 : 0);
    fd = open("/opqdir/fresh", O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) close(fd);
    listdir("/opqdir", buf, sizeof buf);
    printf("g1r_readdir=%s\n", buf);
    printf("g1r_stale_lookup=%d\n", access("/opqdir/stale1", F_OK) < 0 ? 1 : 0);

    // ---- whiteout/unlink semantics: rmdir of a NON-EMPTY lower-backed dir must fail ENOTEMPTY (not
    // silently whiteout-hide the live children). ----
    errno = 0;
    int rmne = rmdir("/rmdne");
    printf("g4b_rmdir_nonempty=%d\n", (rmne < 0 && errno == ENOTEMPTY) ? 1 : 0);
    printf("g4b_child_kept=%s\n", rd("/rmdne/keep", buf, sizeof buf) >= 0 ? buf : "<none>");

    // ---- G7: RENAME_EXCHANGE across layers (lower-only <-> upper-only) must swap BOTH ends ----
    fd = open("/ex_b", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) { write(fd, "BBB", 3); close(fd); }
    int ex = renameat2_("/ex_a", "/ex_b", 2 /*RENAME_EXCHANGE*/);
    printf("g7_exchange_ret=%d\n", ex < 0 ? -1 : 0);
    printf("g7_a=%s\n", rd("/ex_a", buf, sizeof buf) >= 0 ? buf : "<none>");
    printf("g7_b=%s\n", rd("/ex_b", buf, sizeof buf) >= 0 ? buf : "<none>");
    // Harder G7: BOTH ends lower-only. Real overlayfs copies both up, then swaps.
    int ex2 = renameat2_("/ex_c", "/ex_d", 2);
    printf("g7_exchange2_ret=%d\n", ex2 < 0 ? -1 : 0);
    printf("g7_c=%s\n", rd("/ex_c", buf, sizeof buf) >= 0 ? buf : "<none>");
    printf("g7_d=%s\n", rd("/ex_d", buf, sizeof buf) >= 0 ? buf : "<none>");

    printf("PROBE_DONE\n");
    return 0;
}
