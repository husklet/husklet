// F_SETFL mutable-bit contract fixed by the Linux ABI: F_SETFL may change only the file-status flags
// (O_APPEND, O_NONBLOCK, O_DIRECT, O_NOATIME, O_ASYNC); it must IGNORE the access mode and creation
// flags (O_RDONLY/O_WRONLY/O_RDWR, O_CREAT, O_EXCL, O_TRUNC, O_CLOEXEC) rather than let them leak in.
// Verified by round-tripping through F_GETFL. Only booleans are printed, so the golden is host-invariant.
// Arch-neutral.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/null", O_RDWR);
    int base = fcntl(fd, F_GETFL);
    // Access mode reads back as O_RDWR.
    printf("accmode_rdwr=%d\n", (base & O_ACCMODE) == O_RDWR);

    // Try to set O_APPEND|O_NONBLOCK plus garbage creation/access bits.
    fcntl(fd, F_SETFL, O_APPEND | O_NONBLOCK | O_CREAT | O_TRUNC | O_WRONLY);
    int now = fcntl(fd, F_GETFL);
    // Status flags took effect...
    printf("append_on=%d nonblock_on=%d\n", (now & O_APPEND) != 0, (now & O_NONBLOCK) != 0);
    // ...but access mode is unchanged and creation flags never stick in the status word.
    printf("accmode_unchanged=%d creat_ignored=%d trunc_ignored=%d\n",
           (now & O_ACCMODE) == O_RDWR, (now & O_CREAT) == 0, (now & O_TRUNC) == 0);

    // Clearing via F_SETFL(0) drops the status flags again.
    fcntl(fd, F_SETFL, 0);
    int cleared = fcntl(fd, F_GETFL);
    printf("append_cleared=%d nonblock_cleared=%d\n",
           (cleared & O_APPEND) == 0, (cleared & O_NONBLOCK) == 0);
    return 0;
}
