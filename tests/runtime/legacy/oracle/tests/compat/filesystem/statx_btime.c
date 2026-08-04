// statx(2) creation-time and mount-id honesty: stx_mask must advertise a field ONLY when it is
// actually filled. A caller trusts stx_mask before reading stx_btime/stx_mnt_id.
//   - A tmpfs-backed file (/tmp) tracks birth time -> STATX_BTIME set and stx_btime is non-zero, and
//     the same value is reported through a path-based statx and an AT_EMPTY_PATH fd statx.
//   - procfs does not track birth time -> STATX_BTIME must stay clear.
//   - modern kernels fill stx_mnt_id opportunistically, so STATX_MNT_ID is reported for a real file
//     whether or not the caller asked for it, and the value is stable across the two requests.
// All facts are derived booleans (never the dynamic btime/mnt_id values), so the golden is exact.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <linux/stat.h>

#ifndef STATX_MNT_ID
#define STATX_MNT_ID 0x00001000U
#endif

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_statxbt_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "birthtime", 9) != 9) return 1;

    // Path-based statx over the tmpfs file: btime advertised and non-zero.
    struct statx sp;
    memset(&sp, 0, sizeof sp);
    long rp = syscall(__NR_statx, AT_FDCWD, path, 0, STATX_ALL, &sp);
    int btime_mask = (sp.stx_mask & STATX_BTIME) != 0;
    int btime_nonzero = sp.stx_btime.tv_sec != 0 || sp.stx_btime.tv_nsec != 0;

    // AT_EMPTY_PATH fd statx over the same inode: identical btime mask + value.
    struct statx sf;
    memset(&sf, 0, sizeof sf);
    long rf = syscall(__NR_statx, fd, "", AT_EMPTY_PATH, STATX_ALL, &sf);
    int fd_btime_mask = (sf.stx_mask & STATX_BTIME) != 0;
    int btime_consistent = rf == 0 && sf.stx_btime.tv_sec == sp.stx_btime.tv_sec &&
                           sf.stx_btime.tv_nsec == sp.stx_btime.tv_nsec;

    printf("tmp-statx-ok=%d\n", rp == 0 && rf == 0);
    printf("tmp-btime-mask=%d btime-nonzero=%d fd-btime-mask=%d btime-consistent=%d\n",
           btime_mask, btime_nonzero, fd_btime_mask, btime_consistent);

    // procfs does not track birth time -> STATX_BTIME must stay clear.
    struct statx spr;
    memset(&spr, 0, sizeof spr);
    long rpr = syscall(__NR_statx, AT_FDCWD, "/proc/self/stat", 0, STATX_ALL, &spr);
    printf("proc-statx-ok=%d proc-btime-mask=%d\n", rpr == 0, (spr.stx_mask & STATX_BTIME) != 0);

    // mount id: reported for a real file regardless of whether it was requested, same value both ways.
    struct statx su, sr;
    memset(&su, 0, sizeof su);
    memset(&sr, 0, sizeof sr);
    syscall(__NR_statx, AT_FDCWD, path, 0, STATX_ALL, &su);
    syscall(__NR_statx, AT_FDCWD, path, 0, STATX_ALL | STATX_MNT_ID, &sr);
    int mnt_unasked = (su.stx_mask & STATX_MNT_ID) != 0;
    int mnt_asked = (sr.stx_mask & STATX_MNT_ID) != 0;
    int mnt_consistent = mnt_unasked && mnt_asked && su.stx_mnt_id == sr.stx_mnt_id;
    printf("mnt-id-mask-unasked=%d mnt-id-mask-asked=%d mnt-id-consistent=%d\n",
           mnt_unasked, mnt_asked, mnt_consistent);

    close(fd);
    unlink(path);
    return 0;
}
