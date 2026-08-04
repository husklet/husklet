#include "compat.h"
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
/* Pins the S3DB_DURABILITY=none|fast|strict fsync policy (helpers.c s3db_sync_fd), which is otherwise an
 * undocumented env-only switch. The engine routes fsync(82)/fdatasync(83)/sync_file_range(84) through the
 * policy. Two observable facts, host-independent:
 *   1. COHERENCE (all modes): a write + fsync + fdatasync leaves the data readable back (page-cache
 *      coherent), and the syncs report success on a real regular file.
 *   2. NO-OP vs REAL SYNC: `none` "returns success without syncing" -- it returns 0 even for a fd the
 *      kernel would reject (it never issues the real fsync), whereas fast/strict/default perform the real
 *      syscall and fail (-1) on a bad fd, exactly like Linux. This is what makes the mode VISIBLE.
 * The default (no env) run is byte-identical to native Linux, so it is oracle-diffed; the explicit-mode
 * runs are golden (native has no such env var). */
int main(void) {
    char path[] = "/tmp/hl_s3db_XXXXXX";
    int fd = mkstemp(path);
    int regfile_fsync = -2, regfile_fdatasync = -2, readback_ok = 0;
    if (fd >= 0) {
        const char *msg = "durable-payload";
        (void)!write(fd, msg, strlen(msg));
        regfile_fsync = (int)syscall(SYS_fsync, fd);
        regfile_fdatasync = (int)syscall(SYS_fdatasync, fd);
        char buf[64] = {0};
        lseek(fd, 0, SEEK_SET);
        ssize_t n = read(fd, buf, sizeof buf - 1);
        readback_ok = (n == (ssize_t)strlen(msg) && strcmp(buf, msg) == 0);
        close(fd);
        unlink(path);
    }
    /* A deliberately-invalid fd: `none` returns 0 (no real fsync issued -> no fd validation); every other
     * mode issues the real fsync and returns -1 (EBADF), matching Linux. */
    int badfd_fsync_ret = (int)syscall(SYS_fsync, 4242);
    printf("s3db regfile_fsync=%d regfile_fdatasync=%d readback_ok=%d badfd_fsync_ret=%d\n",
           regfile_fsync, regfile_fdatasync, readback_ok, badfd_fsync_ret);
    return 0;
}
