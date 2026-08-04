#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

typedef unsigned char vector16 __attribute__((vector_size(16), aligned(1)));

#if defined(__x86_64__)
typedef unsigned char vector32 __attribute__((vector_size(32), aligned(1)));
#endif

static void store_vector(unsigned char *destination, const unsigned char *source) {
    *(vector16 *)destination = *(const vector16 *)source;
}

static void store_vector32(unsigned char *destination, const unsigned char *source) {
#if defined(__x86_64__)
    /*
     * Keep this as a genuine YMM-width guest store.  The engine deliberately
     * does not advertise AVX through CPUID yet, but its VEX decoder supports
     * binaries which contain AVX instructions selected out of band.
     */
    __asm__ volatile("vmovdqu (%1), %%ymm0\n\t"
                     "vmovdqu %%ymm0, (%0)\n\t"
                     "vzeroupper"
                     :
                     : "r"(destination), "r"(source)
                     : "ymm0", "memory");
#else
    memcpy(destination, source, 32);
#endif
}

static void store_rep(unsigned char *destination, const unsigned char *source, size_t length) {
#if defined(__x86_64__)
    __asm__ volatile("rep movsb" : "+D"(destination), "+S"(source), "+c"(length) : : "memory");
#else
    memcpy(destination, source, length);
#endif
}

static size_t emit_return(unsigned char *code, uint32_t value) {
#if defined(__aarch64__)
    uint32_t words[4] = {
        0x52800000u | ((value & 0xffffu) << 5), /* mov w0, #value */
        0xd503201fu,                            /* nop */
        0xd503201fu,                            /* nop */
        0xd65f03c0u,                            /* ret */
    };
    memcpy(code, words, sizeof(words));
    return sizeof(words);
#elif defined(__x86_64__)
    unsigned char bytes[16] = {
        0xb8, 0,    0,    0,    0,                                        /* mov eax, value */
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xc3, /* ret */
    };
    memcpy(bytes + 1, &value, sizeof(value));
    memcpy(code, bytes, sizeof(bytes));
    return sizeof(bytes);
#else
#error unsupported architecture
#endif
}

static int call_many(const unsigned char *entry, uint32_t expected) {
    uint32_t (*fn)(void) = (uint32_t (*)(void))entry;
    for (int i = 0; i < 64; ++i)
        if (fn() != expected) return 0;
    return 1;
}

int main(void) {
    const size_t page = 4096;
    const size_t length = 2 * page;
    const off_t offset = (off_t)page;
    int fd = (int)syscall(SYS_memfd_create, "exec-alias", 0u);
    if (fd < 0 || ftruncate(fd, offset + (off_t)length) != 0) return 2;

    unsigned char *rw = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, offset);
    /*
     * Offset the fixed RX address by one guest page.  It remains Linux-page
     * aligned while deliberately not sharing a 16 KiB host-page boundary.
     */
    unsigned char *reservation = mmap(NULL, length + page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rw == MAP_FAILED || reservation == MAP_FAILED) return 3;
    unsigned char *fixed = reservation + page;
    unsigned char *rx = mmap(fixed, length, PROT_NONE, MAP_SHARED | MAP_FIXED, fd, offset);
    if (rx != fixed || mprotect(rx, length, PROT_READ | PROT_EXEC) != 0) return 4;
    close(fd);

#if defined(__aarch64__)
    const size_t entry_offset = page - 8;
#else
    const size_t entry_offset = page - 8;
#endif
    unsigned char initial[16], replacement[16], replacement32[32];
    emit_return(initial, 11);
    emit_return(replacement, 33);
    memset(replacement32, 0x90, sizeof(replacement32));
    emit_return(replacement32, 55);

    /* The complete code copy crosses the guest-page boundary. */
    store_vector(rw + entry_offset, initial);
    setbuf(stdout, NULL);
    int visible = memcmp(rx + entry_offset, initial, sizeof(initial)) == 0;
    printf("memfd-exec-alias visible=%d\n", visible);
#if defined(__aarch64__)
    __builtin___clear_cache((char *)rx + entry_offset, (char *)rx + entry_offset + sizeof(initial));
#endif
    int initial_ok = call_many(rx + entry_offset, 11);

    /* Patch only the return-value instruction/immediate through the RW alias. */
#if defined(__aarch64__)
    uint32_t scalar = 0x52800000u | (22u << 5);
    memcpy(rw + entry_offset, &scalar, sizeof(scalar));
#else
    uint32_t scalar = 22;
    memcpy(rw + entry_offset + 1, &scalar, sizeof(scalar));
#endif
    int scalar_visible = memcmp(rx + entry_offset, rw + entry_offset, sizeof(initial)) == 0;
#if defined(__aarch64__)
    /*
     * CoreCLR performs its instruction-cache maintenance through the RX
     * alias.  Do not execute the new body before that architectural
     * maintenance; the engine must first refresh the RX snapshot from its
     * shared backing identity, then invalidate its translated code.
     */
    __builtin___clear_cache((char *)rx + entry_offset, (char *)rx + entry_offset + sizeof(initial));
#endif
    int scalar_exec_ok = call_many(rx + entry_offset, 22);

    /* A compiler-sized aggregate copy rewrites code on both pages at once. */
    store_vector(rw + entry_offset, replacement);
#if defined(__aarch64__)
    __builtin___clear_cache((char *)rx + entry_offset, (char *)rx + entry_offset + sizeof(replacement));
#endif
    int vector_visible = memcmp(rx + entry_offset, replacement, sizeof(replacement)) == 0;
    int cross_page_ok = call_many(rx + entry_offset, 33);

#if defined(__aarch64__)
    uint32_t rep_patch = 0x52800000u | (44u << 5);
    store_rep(rw + entry_offset, (const unsigned char *)&rep_patch, sizeof(rep_patch));
    __builtin___clear_cache((char *)rx + entry_offset, (char *)rx + entry_offset + sizeof(rep_patch));
#else
    uint32_t rep_patch = 44;
    store_rep(rw + entry_offset + 1, (const unsigned char *)&rep_patch, sizeof(rep_patch));
#endif
    int rep_exec_ok = call_many(rx + entry_offset, 44);

    store_vector32(rw + entry_offset, replacement32);
#if defined(__aarch64__)
    __builtin___clear_cache((char *)rx + entry_offset, (char *)rx + entry_offset + sizeof(replacement32));
#endif
    int avx256_visible = memcmp(rx + entry_offset, replacement32, sizeof(replacement32)) == 0;
    int avx256_exec_ok = call_many(rx + entry_offset, 55);

    int unmapped = munmap(rx, length) == 0 && munmap(reservation, page) == 0 && munmap(rw, length) == 0;
    printf("memfd-exec-alias initial=%d scalar-visible=%d scalar-exec=%d "
           "vector-visible=%d cross-page=%d rep-exec=%d avx256-visible=%d "
           "avx256-exec=%d unmapped=%d\n",
           initial_ok, scalar_visible, scalar_exec_ok, vector_visible, cross_page_ok, rep_exec_ok, avx256_visible,
           avx256_exec_ok, unmapped);
    return visible && initial_ok && scalar_visible && scalar_exec_ok && vector_visible && cross_page_ok &&
                   rep_exec_ok && avx256_visible && avx256_exec_ok && unmapped
               ? 0
               : 1;
}
