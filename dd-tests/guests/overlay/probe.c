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

    // ==== M-series (task #169): directory-CREATION ops through the overlay must match real overlayfs, ====
    // never blanket-EPERM. Covers mkdir/mkdirat (plain, nested, over a lower-only parent, over a whiteout,
    // mode/umask) + the sibling create/remove ops that share the jail_at/copy-up path (creat, symlinkat,
    // mknodat, rmdir, unlinkat AT_REMOVEDIR, renameat into the upper) and the real-Linux errno matrix
    // (EEXIST/ENOENT/ENOTDIR). Values were checked against `mount -t overlay`/docker on Linux.

    // M1: plain mkdir of a brand-new name in the writable upper -> 0; a repeat -> EEXIST.
    errno = 0;
    int m1 = mkdir("/m_new", 0755);
    printf("m1_mkdir_new=%d\n", m1 < 0 ? errno : 0);
    errno = 0;
    int m1b = mkdir("/m_new", 0755);
    printf("m1_mkdir_again=%d\n", m1b < 0 ? errno : 0); // expect EEXIST=17

    // M2: mode honored under umask (umask 022; mkdir 0777 -> 0755).
    umask(022);
    mkdir("/m_mode", 0777);
    if (stat("/m_mode", &st) == 0) printf("m2_mode=%o\n", st.st_mode & 07777);
    else printf("m2_mode=stat-fail\n");

    // M3: mkdirat relative to an explicit upper dir-fd.
    int df = open("/m_new", O_RDONLY | O_DIRECTORY);
    errno = 0;
    int m3 = df >= 0 ? mkdirat(df, "sub", 0755) : -1;
    printf("m3_mkdirat=%d\n", m3 < 0 ? errno : 0);
    printf("m3_visible=%d\n", access("/m_new/sub", F_OK) == 0 ? 1 : 0);
    if (df >= 0) close(df);

    // M4: nested mkdir whose PARENT chain lives only in a lower layer (copy-up of the parents).
    // The lower provides /mkp/a/b as a directory; creating /mkp/a/b/newc must succeed and be visible.
    errno = 0;
    int m4 = mkdir("/mkp/a/b/newc", 0755);
    printf("m4_nested_lower_parent=%d\n", m4 < 0 ? errno : 0);
    printf("m4_visible=%d\n", access("/mkp/a/b/newc", F_OK) == 0 ? 1 : 0);

    // M5: mkdir under a NONEXISTENT parent -> ENOENT (not EPERM).
    errno = 0;
    int m5 = mkdir("/no_such_parent/child", 0755);
    printf("m5_enoent=%d\n", (m5 < 0 && errno == ENOENT) ? 1 : errno);

    // M6: mkdir where a path component is a regular file (lower-only /mnotdir) -> ENOTDIR.
    errno = 0;
    int m6 = mkdir("/mnotdir/child", 0755);
    printf("m6_enotdir=%d\n", (m6 < 0 && errno == ENOTDIR) ? 1 : errno);

    // M7: mkdir over a WHITEOUT — a lower-only file removed, then a dir created with the same name.
    unlink("/wf"); // /wf is a lower regular file -> drops a whiteout
    errno = 0;
    int m7 = mkdir("/wf", 0755);
    printf("m7_mkdir_over_wh=%d\n", m7 < 0 ? errno : 0);
    printf("m7_isdir=%d\n", (stat("/wf", &st) == 0 && S_ISDIR(st.st_mode)) ? 1 : 0);

    // M8: sibling create ops into an overlay upper dir — creat, symlinkat, mknodat(FIFO).
    errno = 0;
    int cf = creat("/m_new/cf", 0644);
    printf("m8_creat=%d\n", cf < 0 ? errno : 0);
    if (cf >= 0) close(cf);
    errno = 0;
    int sl = symlink("target", "/m_new/sl");
    printf("m8_symlink=%d\n", sl < 0 ? errno : 0);
    errno = 0;
    int mn = mknod("/m_new/fifo", S_IFIFO | 0644, 0);
    printf("m8_mknod_fifo=%d\n", mn < 0 ? errno : 0);

    // M9: rmdir of an upper-created empty dir -> 0; a repeat -> ENOENT.
    mkdir("/m_rmdir", 0755);
    errno = 0;
    int r9 = rmdir("/m_rmdir");
    printf("m9_rmdir=%d\n", r9 < 0 ? errno : 0);
    errno = 0;
    int r9b = rmdir("/m_rmdir");
    printf("m9_rmdir_gone=%d\n", (r9b < 0 && errno == ENOENT) ? 1 : errno);

    // M10: rmdir a regular file -> ENOTDIR; unlinkat AT_REMOVEDIR on a dir -> 0.
    errno = 0;
    int r10 = rmdir("/mnotdir");
    printf("m10_rmdir_file=%d\n", (r10 < 0 && errno == ENOTDIR) ? 1 : errno);
    mkdir("/m_rd2", 0755);
    errno = 0;
    int r10b = unlinkat(AT_FDCWD, "/m_rd2", AT_REMOVEDIR);
    printf("m10_unlinkat_rmdir=%d\n", r10b < 0 ? errno : 0);

    // M11: renameat a fresh upper file into a lower-only (copied-up) directory -> 0, then readable there.
    fd = open("/m_ren_src", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) { write(fd, "REN", 3); close(fd); }
    errno = 0;
    int m11 = rename("/m_ren_src", "/mkp/a/b/ren_dst");
    printf("m11_rename_into_overlay=%d\n", m11 < 0 ? errno : 0);
    printf("m11_dst=%s\n", rd("/mkp/a/b/ren_dst", buf, sizeof buf) >= 0 ? buf : "<none>");

    // M12: mkdir where the FINAL name already exists in a LOWER layer -> EEXIST (dir or file).
    errno = 0;
    int m12a = mkdir("/ld_pristine", 0755); // a pristine lower-only directory (never copied up)
    printf("m12_mkdir_over_lowerdir=%d\n", (m12a < 0 && errno == EEXIST) ? 1 : errno);
    errno = 0;
    int m12b = mkdir("/mnotdir", 0755); // /mnotdir is a lower-only regular file
    printf("m12_mkdir_over_lowerfile=%d\n", (m12b < 0 && errno == EEXIST) ? 1 : errno);

    // M13: symlink/creat where the name already exists in a lower layer -> EEXIST.
    errno = 0;
    int m13a = symlink("x", "/mnotdir");
    printf("m13_symlink_eexist=%d\n", (m13a < 0 && errno == EEXIST) ? 1 : errno);
    errno = 0;
    int m13b = open("/mkp", O_CREAT | O_EXCL | O_WRONLY, 0644);
    printf("m13_openexcl_eexist=%d\n", (m13b < 0 && errno == EEXIST) ? 1 : (m13b >= 0 ? (close(m13b), -100) : errno));

    // M14: rmdir a lower-only EMPTY directory -> success (real overlayfs drops a whiteout), then gone.
    errno = 0;
    int m14 = rmdir("/mkp_empty"); // lower-only empty dir
    printf("m14_rmdir_lower_empty=%d\n", m14 < 0 ? errno : 0);
    printf("m14_gone=%d\n", access("/mkp_empty", F_OK) < 0 ? 1 : 0);

    // M15: mkdirat where the dir-fd points at a lower-only (copied-up) directory.
    int df2 = open("/mkp/a", O_RDONLY | O_DIRECTORY); // /mkp/a is lower-only
    errno = 0;
    int m15 = df2 >= 0 ? mkdirat(df2, "atchild", 0755) : -1;
    printf("m15_mkdirat_lower_dirfd=%d\n", df2 < 0 ? -1 : (m15 < 0 ? errno : 0));
    printf("m15_visible=%d\n", access("/mkp/a/atchild", F_OK) == 0 ? 1 : 0);
    if (df2 >= 0) close(df2);

    // ==== N-series (#239/#269): STALE POSITIVE after removing a lower-backed directory ====
    // Real overlayfs whiteouts the removed dir, which hides EVERY read-only lower child beneath it: a later
    // stat/access of a child must be ENOENT. dd resolved a whole path inside one layer, so it kept finding
    // the lower child through the whited-out parent -> the child wrongly stat'd as present after `rm -r`
    // (#239), and a merged view under an opaque/removed parent leaked stale lower entries (#269). Values
    // verified against `docker` on real overlayfs (fixtures baked into an image lower layer).

    // N1: flat lower-backed dir removed; warm the positive caches FIRST (so a precise-evict miss would show).
    access("/rmparent", F_OK);
    access("/rmparent/c1", F_OK);
    (void)stat("/rmparent/c1", &st);
    unlink("/rmparent/c1");
    unlink("/rmparent/c2");
    rmdir("/rmparent");
    printf("n1_parent_gone=%d\n", access("/rmparent", F_OK) < 0 ? 1 : 0);
    printf("n1_child_access=%d\n", access("/rmparent/c1", F_OK) < 0 ? 1 : 0);
    printf("n1_child_stat=%d\n", stat("/rmparent/c1", &st) < 0 ? 1 : 0);

    // N2: a DEEP lower-only subtree removed bottom-up; no descendant may stale-resolve after the top is gone.
    access("/rmdeep/a/b/c/leaf", F_OK);
    unlink("/rmdeep/a/b/c/leaf");
    rmdir("/rmdeep/a/b/c");
    rmdir("/rmdeep/a/b");
    rmdir("/rmdeep/a");
    printf("n2_leaf_gone=%d\n", access("/rmdeep/a/b/c/leaf", F_OK) < 0 ? 1 : 0);
    printf("n2_midc_gone=%d\n", access("/rmdeep/a/b/c", F_OK) < 0 ? 1 : 0);
    printf("n2_top_gone=%d\n", access("/rmdeep/a", F_OK) < 0 ? 1 : 0);

    // N3 (#269): a lower-backed dir with a copied-up child AND a lower child; empty it, then rmdir must
    // SUCCEED (no ENOTEMPTY from leftover upper copy / `.wh.` markers) and a later child stat is ENOENT.
    fd = open("/rmcopy/up", O_CREAT | O_WRONLY, 0644); // brand-new upper child
    if (fd >= 0) close(fd);
    fd = open("/rmcopy/k1", O_WRONLY); // write-open copies the lower child up into the upper
    if (fd >= 0) close(fd);
    unlink("/rmcopy/up");
    unlink("/rmcopy/k1");
    errno = 0;
    printf("n3_rmdir=%d\n", rmdir("/rmcopy") == 0 ? 0 : errno);
    printf("n3_child_gone=%d\n", access("/rmcopy/k1", F_OK) < 0 ? 1 : 0);

    printf("PROBE_DONE\n");
    return 0;
}
