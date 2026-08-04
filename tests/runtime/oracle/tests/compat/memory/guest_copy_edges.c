#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static int fill(int descriptor, unsigned char seed, size_t length) {
    unsigned char bytes[4096];
    for (size_t offset = 0; offset < length;) {
        size_t chunk = length - offset < sizeof bytes ? length - offset : sizeof bytes;
        for (size_t index = 0; index < chunk; ++index) bytes[index] = (unsigned char)(seed + offset + index);
        if (write(descriptor, bytes, chunk) != (ssize_t)chunk) return 0;
        offset += chunk;
    }
    return 1;
}

static int call_code(const unsigned char *code) {
    return ((int (*)(void))(uintptr_t)code)();
}

int main(void) {
    const size_t page = 4096;
    int backing = (int)syscall(SYS_memfd_create, "guest-copy-edges", 0u);
    if (backing < 0 || ftruncate(backing, 4 * (off_t)page) != 0) return 2;

    unsigned char *area = mmap(NULL, 5 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED) return 3;
    unsigned char *logical0 =
        mmap(area + page, page, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_SHARED, backing, page);
    unsigned char *direct =
        mmap(area + 2 * page, page, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned char *logical1 =
        mmap(area + 3 * page, page, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_SHARED, backing, 2 * page);
    if (logical0 != area + page || direct != area + 2 * page || logical1 != area + 3 * page) return 4;

    int pipefd[2];
    if (pipe(pipefd) != 0 || !fill(pipefd[1], 19, 3 * page)) return 5;
    ssize_t three = read(pipefd[0], logical0, 3 * page);
    int three_ok = three == (ssize_t)(3 * page) && logical0[0] == 19 &&
                   direct[page - 1] == (unsigned char)(18) && logical1[page - 1] == (unsigned char)(18);
    close(pipefd[0]);
    close(pipefd[1]);

    struct iovec *vectors = mmap(NULL, 1025 * sizeof(*vectors), PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (vectors == MAP_FAILED) return 6;
    for (size_t index = 0; index < 1025; ++index) vectors[index] = (struct iovec){logical0, 0};
    vectors[1023] = (struct iovec){logical0, 1};
    if (pipe(pipefd) != 0) return 7;
    errno = 0;
    ssize_t boundary = writev(pipefd[1], vectors, 1024);
    int boundary_ok = boundary == 1;
    errno = 0;
    ssize_t over_count = writev(pipefd[1], vectors, 1025);
    int over_count_ok = over_count == -1 && errno == EINVAL;
    errno = 0;
    ssize_t overflow = syscall(SYS_writev, pipefd[1], (void *)(uintptr_t)(UINTPTR_MAX - 7), 1025);
    int overflow_ok = overflow == -1 && errno == EINVAL;
    close(pipefd[0]);
    close(pipefd[1]);

    int codefd = (int)syscall(SYS_memfd_create, "guest-copy-smc", 0u);
    if (codefd < 0 || ftruncate(codefd, 2 * (off_t)page) != 0) return 8;
    unsigned char *writable = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_SHARED, codefd, 0);
    unsigned char *executable = mmap(NULL, page, PROT_READ | PROT_EXEC, MAP_SHARED, codefd, 0);
    if (writable == MAP_FAILED || executable == MAP_FAILED) return 9;
#if defined(__aarch64__)
    static const unsigned char one[] = {0x20, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6};
    static const unsigned char two[] = {0x40, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6};
#elif defined(__x86_64__)
    static const unsigned char one[] = {0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3};
    static const unsigned char two[] = {0xb8, 0x02, 0x00, 0x00, 0x00, 0xc3};
#else
#error unsupported guest ISA
#endif
    memcpy(writable, one, sizeof one);
    int before = call_code(executable);
    unsigned char replacement[4096] = {0};
    memcpy(replacement, two, sizeof two);
    if (mprotect(writable + page, page, PROT_NONE) != 0 || pipe(pipefd) != 0 ||
        write(pipefd[1], replacement, sizeof replacement) != (ssize_t)sizeof replacement ||
        write(pipefd[1], replacement, sizeof replacement) != (ssize_t)sizeof replacement)
        return 10;
    errno = 0;
    ssize_t partial = read(pipefd[0], writable, 2 * page);
#if defined(__aarch64__)
    /*
     * Native AArch64 requires an explicit instruction-cache synchronization
     * after publishing code. The engine's syscall copyout path must already
     * have retired executable aliases; this keeps the same source usable as a
     * native Linux oracle without weakening x86's implicit-coherency check.
     */
    __builtin___clear_cache((char *)executable, (char *)(executable + sizeof two));
#endif
    int after = call_code(executable);
    int smc_ok = before == 1 && partial == (ssize_t)page && after == 2;
    close(pipefd[0]);
    close(pipefd[1]);

    unsigned char *readonly = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (readonly == MAP_FAILED) return 11;
    readonly[0] = 0x5a;
    if (mprotect(readonly, page, PROT_READ) != 0 || pipe(pipefd) != 0 || write(pipefd[1], "x", 1) != 1) return 12;
    errno = 0;
    ssize_t readonly_result = read(pipefd[0], readonly, 1);
    int readonly_ok = readonly_result == -1 && errno == EFAULT && readonly[0] == 0x5a;
    close(pipefd[0]);
    close(pipefd[1]);

    printf("guest-copy-edges three-span=%d iov-boundary=%d iov-over=%d iov-overflow=%d readonly=%d smc=%d\n",
           three_ok, boundary_ok, over_count_ok, overflow_ok, readonly_ok, smc_ok);
    return !(three_ok && boundary_ok && over_count_ok && overflow_ok && readonly_ok && smc_ok);
}
