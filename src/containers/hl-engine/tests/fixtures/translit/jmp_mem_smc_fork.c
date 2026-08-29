#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static uint64_t target_a(void) { return UINT64_C(0xaaaaaaaaaaaaaaaa); }
static uint64_t target_b(void) { return UINT64_C(0xbbbbbbbbbbbbbbbb); }
typedef uint64_t (*probe_fn)(const uint64_t *, const uint64_t *);

static uint64_t repeat(probe_fn fn, const uint64_t *a, const uint64_t *b, uint64_t expected) {
    uint64_t value = 0;
    for (int i = 0; i < 64; i++) value = fn(a, b);
    return value == expected;
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    unsigned char *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) return 2;
    /* FF 27 = jmp *(%rdi); FF 26 = jmp *(%rsi). */
    code[0] = 0xff;
    code[1] = 0x27;
    if (mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) != 0) return 3;
    probe_fn fn;
    memcpy(&fn, &code, sizeof fn);
    uint64_t a = (uint64_t)(uintptr_t)target_a, b = (uint64_t)(uintptr_t)target_b;
    int before = repeat(fn, &a, &b, UINT64_C(0xaaaaaaaaaaaaaaaa));

    if (mprotect(code, (size_t)page, PROT_READ | PROT_WRITE) != 0) return 4;
    code[1] = 0x26;
    __builtin___clear_cache((char *)code, (char *)code + 2);
    if (mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) != 0) return 5;
    int smc = repeat(fn, &a, &b, UINT64_C(0xbbbbbbbbbbbbbbbb));

    int pipefd[2];
    if (pipe(pipefd) != 0) return 6;
    pid_t child = fork();
    if (child < 0) return 7;
    if (child == 0) {
        close(pipefd[0]);
        unsigned char result = repeat(fn, &a, &b, UINT64_C(0xbbbbbbbbbbbbbbbb));
        (void)write(pipefd[1], &result, 1);
        _exit(result ? 0 : 8);
    }
    close(pipefd[1]);
    unsigned char child_result = 0;
    int status = 0;
    ssize_t got = read(pipefd[0], &child_result, 1);
    waitpid(child, &status, 0);
    int parent = repeat(fn, &a, &b, UINT64_C(0xbbbbbbbbbbbbbbbb));
    int forked = got == 1 && child_result == 1 && WIFEXITED(status) && WEXITSTATUS(status) == 0 && parent;
    printf("before=%d smc=%d fork=%d\n", before, smc, forked);
    return before && smc && forked ? 0 : 9;
}
