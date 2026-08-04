#define _GNU_SOURCE
#include <fcntl.h>
#include <linux/aio_abi.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int setup(unsigned count, aio_context_t *context) {
    return (int)syscall(SYS_io_setup, count, context);
}

static int submit(aio_context_t context, long count, struct iocb **items) {
    return (int)syscall(SYS_io_submit, context, count, items);
}

static int events(aio_context_t context, long minimum, long count, struct io_event *items) {
    return (int)syscall(SYS_io_getevents, context, minimum, count, items, NULL);
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "verify") == 0) {
        unsigned char data[8 * 4096];
        int fd = open(argv[2], O_RDONLY);
        int ok = fd >= 0 && read(fd, data, sizeof data) == (ssize_t)sizeof data;
        for (size_t page = 0; ok && page < 8; page++)
            for (size_t byte = 0; byte < 4096; byte++)
                if (data[page * 4096 + byte] != (unsigned char)(page + 1)) ok = 0;
        if (fd >= 0) close(fd);
        return ok ? 0 : 1;
    }
    char old_path[128], new_path[128];
    snprintf(old_path, sizeof old_path, "/tmp/hl-aio-persist-%ld", (long)getpid());
    snprintf(new_path, sizeof new_path, "%s-renamed", old_path);
    int fd = open(old_path, O_CREAT | O_EXCL | O_RDWR, 0600);
    int signal_fd = eventfd(0, EFD_CLOEXEC);
    aio_context_t context = 0;
    if (fd < 0 || signal_fd < 0 || setup(8, &context) != 0) return 1;
    unsigned char data[8][4096];
    struct iocb controls[8], *pointers[8];
    memset(controls, 0, sizeof controls);
    for (size_t index = 0; index < 8; index++) {
        memset(data[index], (int)index + 1, sizeof data[index]);
        controls[index].aio_data = UINT64_C(0xabc000) + index;
        controls[index].aio_lio_opcode = IOCB_CMD_PWRITE;
        controls[index].aio_fildes = (uint32_t)fd;
        controls[index].aio_buf = (uint64_t)(uintptr_t)data[index];
        controls[index].aio_nbytes = sizeof data[index];
        controls[index].aio_offset = (int64_t)(index * sizeof data[index]);
        controls[index].aio_flags = IOCB_FLAG_RESFD;
        controls[index].aio_resfd = (uint32_t)signal_fd;
        pointers[index] = &controls[index];
    }
    int submitted = submit(context, 8, pointers);
    uint64_t signaled = 0;
    int signal_ok = read(signal_fd, &signaled, sizeof signaled) == sizeof signaled && signaled == 8;
    struct io_event completed[8];
    memset(completed, 0, sizeof completed);
    int count = events(context, 8, 8, completed);
    unsigned seen = 0;
    int completion_ok = count == 8;
    for (int index = 0; completion_ok && index < count; index++) {
        uint64_t id = completed[index].data - UINT64_C(0xabc000);
        if (id >= 8 || completed[index].obj != (uint64_t)(uintptr_t)&controls[id] || completed[index].res != 4096 ||
            completed[index].res2 != 0 || (seen & (1u << id)))
            completion_ok = 0;
        seen |= 1u << id;
    }
    int durable = fsync(fd) == 0 && close(fd) == 0 && rename(old_path, new_path) == 0;
    pid_t child = fork();
    if (child == 0) {
        execl("/proc/self/exe", "aio-persist", "verify", new_path, NULL);
        _exit(2);
    }
    int status = 0;
    int reopened = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    syscall(SYS_io_destroy, context);
    close(signal_fd);
    unlink(new_path);
    printf("aio-persist submit=%d signal=%d completions=%d durable=%d reopen=%d\n", submitted, signal_ok,
           completion_ok && seen == 0xffu, durable, reopened);
    return submitted == 8 && signal_ok && completion_ok && seen == 0xffu && durable && reopened ? 0 : 1;
}
