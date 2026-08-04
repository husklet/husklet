#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

#if defined(__x86_64__)
static sigjmp_buf jump;
static volatile uint64_t fault_rcx;
static volatile uintptr_t fault_rsi;
static volatile uintptr_t fault_rdi;

static void fault_handler(int signal, siginfo_t *info, void *opaque) {
    (void)signal;
    (void)info;
    ucontext_t *context = opaque;
    fault_rcx = (uint64_t)context->uc_mcontext.gregs[REG_RCX];
    fault_rsi = (uintptr_t)context->uc_mcontext.gregs[REG_RSI];
    fault_rdi = (uintptr_t)context->uc_mcontext.gregs[REG_RDI];
    __asm__ volatile("cld" ::: "cc");
    siglongjmp(jump, 1);
}

static int copy_fault(unsigned char *destination, unsigned char *source, size_t length, int backward) {
    if (sigsetjmp(jump, 1) == 0) {
        if (backward)
            __asm__ volatile("std; rep movsb; cld"
                             : "+D"(destination), "+S"(source), "+c"(length)
                             :
                             : "memory", "cc");
        else
            __asm__ volatile("cld; rep movsb"
                             : "+D"(destination), "+S"(source), "+c"(length)
                             :
                             : "memory", "cc");
        return 0;
    }
    return 1;
}

int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "rep-logical-fault", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(4 * page)) != 0) return 2;
    unsigned char *reservation = mmap(NULL, 6 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return 3;
    unsigned char *source = reservation + page;
    unsigned char *destination = reservation + 3 * page;
    if (mmap(source, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, (off_t)page) != source ||
        mmap(source + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, (off_t)(2 * page)) !=
            source + page ||
        mmap(destination, 2 * page, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) != destination)
        return 3;

    struct sigaction action = {.sa_sigaction = fault_handler, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0 || sigaction(SIGBUS, &action, NULL) != 0) return 4;

    memset(source, 0, 2 * page);
    memcpy(source + page - 4, "ABCD", 4);
    memset(destination, 0, 8);
    if (mprotect(source + page, page, PROT_NONE) != 0) return 5;
    int forward = copy_fault(destination, source + page - 4, 8, 0) &&
                  memcmp(destination, "ABCD", 4) == 0 && fault_rcx == 4 &&
                  fault_rsi == (uintptr_t)(source + page) &&
                  fault_rdi == (uintptr_t)(destination + 4);

    if (mprotect(source, page, PROT_NONE) != 0 ||
        mprotect(source + page, page, PROT_READ | PROT_WRITE) != 0)
        return 6;
    memcpy(source + page, "WXYZ", 4);
    memset(destination, 0, 8);
    int backward = copy_fault(destination + 7, source + page + 3, 8, 1) &&
                   memcmp(destination + 4, "WXYZ", 4) == 0 && fault_rcx == 4 &&
                   fault_rsi == (uintptr_t)(source + page - 1) &&
                   fault_rdi == (uintptr_t)(destination + 3);

    unsigned char overlap1[] = "abcdefgh";
    unsigned char overlap2[] = "abcdefgh";
    size_t six = 6;
    unsigned char *d1 = overlap1 + 2, *s1 = overlap1;
    __asm__ volatile("cld; rep movsb" : "+D"(d1), "+S"(s1), "+c"(six) : : "memory", "cc");
    six = 6;
    unsigned char *d2 = overlap2 + 7, *s2 = overlap2 + 5;
    __asm__ volatile("std; rep movsb; cld" : "+D"(d2), "+S"(s2), "+c"(six) : : "memory", "cc");
    int overlap_forward = memcmp(overlap1, "abababab", 8) == 0;
    int overlap_backward = memcmp(overlap2, "ababcdef", 8) == 0;

    printf("x86-rep-logical forward=%d backward=%d overlap-fwd=%d overlap-back=%d\n",
           forward, backward, overlap_forward, overlap_backward);
    return !(forward && backward && overlap_forward && overlap_backward);
}
#else
int main(void) {
    return 0;
}
#endif
