#define _GNU_SOURCE
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#if defined(__aarch64__)
static int fault_exit(int status) {
    return (WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV) ||
           (WIFEXITED(status) && WEXITSTATUS(status) == 139);
}

int main(void) {
    const size_t page = 4096;
    int first_fd = (int)syscall(SYS_memfd_create, "cross-first", 0u);
    int second_fd = (int)syscall(SYS_memfd_create, "cross-second", 0u);
    if (first_fd < 0 || second_fd < 0 ||
        ftruncate(first_fd, (off_t)(page * 2)) != 0 ||
        ftruncate(second_fd, (off_t)(page * 2)) != 0) return 2;
    unsigned char *range =
        mmap(NULL, page * 2, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (range == MAP_FAILED) return 3;
    if (mmap(range, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED,
             first_fd, (off_t)page) != range ||
        mmap(range + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED,
             second_fd, (off_t)page) != range + page) return 4;
    volatile unsigned char *bytes = range;
    for (int i = 0; i < 4; ++i) {
        bytes[page - 4 + i] = (unsigned char)(0x10 + i);
        bytes[page + i] = (unsigned char)(0x20 + i);
    }
    uint64_t value;
    __asm__ volatile("ldr %x[value], [%x[address]]"
                     : [value] "=r"(value)
                     : [address] "r"(range + page - 4)
                     : "memory");
    int ok = value == UINT64_C(0x2322212013121110);
    uint64_t replacement = UINT64_C(0xa7a6a5a4a3a2a1a0);
    __asm__ volatile("str %x[value], [%x[address]]"
                     :
                     : [value] "r"(replacement), [address] "r"(range + page - 4)
                     : "memory");
    uint64_t stored = 0;
    for (int i = 0; i < 8; ++i)
        stored |= (uint64_t)bytes[page - 4 + i] << (i * 8);
    int store_ok = stored == replacement;
    unsigned char vector_out[16], vector_new[16];
    for (int i = 0; i < 16; ++i) {
        bytes[page - 8 + i] = (unsigned char)(0x30 + i);
        vector_new[i] = (unsigned char)(0x80 + i);
    }
    __asm__ volatile("ldr q0, [%x[address]]\n"
                     "str q0, [%x[out]]"
                     :
                     : [address] "r"(range + page - 8), [out] "r"(vector_out)
                     : "v0", "memory");
    int vector_load = 1;
    for (int i = 0; i < 16; ++i)
        if (vector_out[i] != (unsigned char)(0x30 + i)) vector_load = 0;
    __asm__ volatile("ldr q1, [%x[in]]\n"
                     "str q1, [%x[address]]"
                     :
                     : [in] "r"(vector_new), [address] "r"(range + page - 8)
                     : "v1", "memory");
    int vector_store = 1;
    for (int i = 0; i < 16; ++i)
        if (bytes[page - 8 + i] != vector_new[i]) vector_store = 0;
    uint64_t pair_first, pair_second;
    __asm__ volatile("ldp %x[first], %x[second], [%x[address]]"
                     : [first] "=r"(pair_first), [second] "=r"(pair_second)
                     : [address] "r"(range + page - 8)
                     : "memory");
    int pair_load = pair_first == UINT64_C(0x8786858483828180) &&
                    pair_second == UINT64_C(0x8f8e8d8c8b8a8988);
    uint64_t pair_new_first = UINT64_C(0xb7b6b5b4b3b2b1b0);
    uint64_t pair_new_second = UINT64_C(0xbfbebdbcbbbab9b8);
    __asm__ volatile("stp %x[first], %x[second], [%x[address]]"
                     :
                     : [first] "r"(pair_new_first), [second] "r"(pair_new_second),
                       [address] "r"(range + page - 8)
                     : "memory");
    int pair_store = 1;
    for (int i = 0; i < 16; ++i)
        if (bytes[page - 8 + i] != (unsigned char)(0xb0 + i)) pair_store = 0;
    unsigned char structure_out[16];
    __asm__ volatile("ld1 {v2.16b}, [%x[address]]\n"
                     "str q2, [%x[out]]"
                     :
                     : [address] "r"(range + page - 8), [out] "r"(structure_out)
                     : "v2", "memory");
    int structure_load = 1;
    for (int i = 0; i < 16; ++i)
        if (structure_out[i] != (unsigned char)(0xb0 + i)) structure_load = 0;
    __asm__ volatile("ldr q3, [%x[in]]\n"
                     "st1 {v3.16b}, [%x[address]]"
                     :
                     : [in] "r"(vector_new), [address] "r"(range + page - 8)
                     : "v3", "memory");
    int structure_store = 1;
    for (int i = 0; i < 16; ++i)
        if (bytes[page - 8 + i] != vector_new[i]) structure_store = 0;
    unsigned char before[8];
    for (int i = 0; i < 8; ++i) before[i] = bytes[page - 4 + i];
    pid_t second_child = fork();
    if (second_child == 0) {
        if (mprotect(range + page, page, PROT_NONE) != 0) _exit(10);
        uint64_t fault_value = UINT64_C(0xdeadbeefcafef00d);
        __asm__ volatile("str %x[value], [%x[address]]"
                         :
                         : [value] "r"(fault_value), [address] "r"(range + page - 4)
                         : "memory");
        _exit(0);
    }
    int second_status = 0;
    waitpid(second_child, &second_status, 0);
    int second_fault = fault_exit(second_status);
    int no_partial_store = 1;
    for (int i = 0; i < 8; ++i)
        if (bytes[page - 4 + i] != before[i]) no_partial_store = 0;
    pid_t first_child = fork();
    if (first_child == 0) {
        if (mprotect(range, page, PROT_NONE) != 0) _exit(10);
        uint64_t ignored;
        __asm__ volatile("ldr %x[value], [%x[address]]"
                         : [value] "=r"(ignored)
                         : [address] "r"(range + page - 4)
                         : "memory");
        _exit((int)ignored & 1);
    }
    int first_status = 0;
    waitpid(first_child, &first_status, 0);
    int first_fault = fault_exit(first_status);
    printf("aarch64-cross-view load=%d store=%d vector-load=%d vector-store=%d pair-load=%d pair-store=%d structure-load=%d structure-store=%d first-fault=%d second-fault=%d no-partial-store=%d\n",
           ok, store_ok, vector_load, vector_store, pair_load, pair_store, structure_load,
           structure_store, first_fault, second_fault, no_partial_store);
    return ok && store_ok && vector_load && vector_store && pair_load && pair_store &&
                   structure_load && structure_store && first_fault && second_fault &&
                   no_partial_store
               ? 0
               : 1;
}
#else
int main(void) { return 0; }
#endif
