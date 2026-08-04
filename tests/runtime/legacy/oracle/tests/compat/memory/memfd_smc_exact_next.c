#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static size_t emit_return(unsigned char *code, uint32_t value) {
    unsigned char bytes[16] = {
        0xb8, 0, 0, 0, 0,
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xc3,
    };
    memcpy(bytes + 1, &value, sizeof(value));
    memcpy(code, bytes, sizeof(bytes));
    return sizeof(bytes);
}

int main(void) {
#if !defined(__x86_64__)
    puts("memfd-smc-exact-next skipped=1");
    return 0;
#else
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "smc-exact-next", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)page) != 0) return 2;
    unsigned char *rw = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *rx = mmap(NULL, page, PROT_READ | PROT_EXEC, MAP_SHARED, fd, 0);
    close(fd);
    if (rw == MAP_FAILED || rx == MAP_FAILED) return 3;

    unsigned char initial[16], replacement[16];
    emit_return(initial, 11);
    emit_return(replacement, 77);
    memcpy(rw, initial, sizeof(initial));
    uint32_t (*fn)(void) = (uint32_t (*)(void))rx;
    int initial_ok = fn() == 11;

    /*
     * Both counters live in the same MAP_SHARED object as an executable
     * alias.  Their stores therefore queue SMC observation even though their
     * byte ranges do not overlap the translated function.  Advancing RIP too
     * early, or replaying the completed store, changes either counter.
     */
    volatile uint32_t *scalar = (volatile uint32_t *)(rw + 128);
    volatile uint32_t *locked = (volatile uint32_t *)(rw + 132);
    *scalar = 0;
    *locked = 0;
    (*scalar)++;
    uint32_t old = __atomic_fetch_add(locked, 1u, __ATOMIC_SEQ_CST);
    int scalar_once = *scalar == 1;
    int lock_once = old == 0 && *locked == 1;

    unsigned char *dst = rw;
    const unsigned char *src = replacement;
    size_t count = sizeof(replacement);
    unsigned char *dst_after;
    const unsigned char *src_after;
    size_t count_after;
    __asm__ volatile("rep movsb"
                     : "=D"(dst_after), "=S"(src_after), "=c"(count_after)
                     : "0"(dst), "1"(src), "2"(count)
                     : "memory");
    int rep_remainder = memcmp(rw, replacement, sizeof(replacement)) == 0;
    int rep_state = count_after == 0 && dst_after == dst + sizeof(replacement) &&
                    src_after == src + sizeof(replacement);
    int next_rip = fn() == 77;

    printf("memfd-smc-exact-next initial=%d scalar-once=%d lock-once=%d "
           "rep-remainder=%d rep-state=%d next-rip=%d\n",
           initial_ok, scalar_once, lock_once, rep_remainder, rep_state, next_rip);
    munmap(rx, page);
    munmap(rw, page);
    return initial_ok && scalar_once && lock_once && rep_remainder && rep_state && next_rip ? 0 : 1;
#endif
}
