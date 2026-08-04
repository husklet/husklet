// Kernel-AIO io_submit opcode validation via RAW syscalls. Linux validates aio_lio_opcode
// synchronously inside io_submit: an unsupported opcode makes io_submit ITSELF fail with EINVAL
// (returning the count of successfully submitted iocbs, or -EINVAL when none were), rather than
// accepting the request and reporting the error later as an io_getevents completion. A synchronous
// emulation that instead queues a completion for the bad opcode would return io_submit=1 here and
// leak a bogus event, so this pins the kernel-accurate contract on both guest ISAs.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/aio_abi.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    aio_context_t ctx = 0;
    if (syscall(SYS_io_setup, 8u, &ctx) != 0) {
        printf("setup fail\n");
        return 1;
    }

    int fd = open("/dev/null", O_RDWR);
    char byte = 0;

    // 1) Unsupported opcode as the sole iocb -> io_submit fails synchronously with EINVAL.
    struct iocb bad = {0};
    bad.aio_lio_opcode = 0x7fff; // not a real IOCB_CMD_*
    bad.aio_fildes = (uint32_t)fd;
    bad.aio_buf = (uint64_t)(uintptr_t)&byte;
    bad.aio_nbytes = 1;
    struct iocb *only_bad[1] = {&bad};
    long r = syscall(SYS_io_submit, ctx, 1L, only_bad);
    printf("bad_opcode_submit=%ld errno=%d\n", r, r == -1 ? errno : 0);

    // No completion may have been queued: a non-blocking reap must find nothing.
    struct io_event ev[2];
    struct timespec zero = {0, 0};
    long g = syscall(SYS_io_getevents, ctx, 0L, 2L, ev, &zero);
    printf("bad_opcode_events=%ld\n", g);

    // 2) A valid iocb AFTER a bad one in the same batch: Linux submits the good prefix, then stops at
    //    the bad opcode and reports the count already submitted (1) rather than -EINVAL.
    struct iocb good = {0};
    good.aio_lio_opcode = IOCB_CMD_PWRITE;
    good.aio_fildes = (uint32_t)fd;
    good.aio_buf = (uint64_t)(uintptr_t)&byte;
    good.aio_nbytes = 1;
    good.aio_offset = 0;
    good.aio_data = 0xabcdULL;
    struct iocb *good_then_bad[2] = {&good, &bad};
    r = syscall(SYS_io_submit, ctx, 2L, good_then_bad);
    printf("prefix_submit=%ld\n", r);
    g = syscall(SYS_io_getevents, ctx, 1L, 2L, ev, &zero);
    printf("prefix_events=%ld data0=%llx\n", g, g >= 1 ? (unsigned long long)ev[0].data : 0);

    syscall(SYS_io_destroy, ctx);
    close(fd);
    return 0;
}
