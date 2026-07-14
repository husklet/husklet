// pcachex/libmap.c -- #178 selective-restore / deferred-activation exerciser.
//
// A shared library's code lives in a file-backed, non-fixed, PROT_EXEC mmap that the kernel places at an
// address that used to move run-to-run -- so its translated blocks were dead weight in a warm restore
// (and a latent stale-translation hazard). #178 gives such maps a DETERMINISTIC base hint and records a
// {base,len,file-identity} manifest; a warm run DEFERS those blocks and only activates them when the same
// file identity re-maps at the same base. This guest is a self-contained stand-in for that path -- it
// runs BARE (static-pie, no rootfs / no real libc.so needed): it materializes a tiny leaf function into a
// STABLE file (written once; unchanged identity across runs), maps it file-backed+exec, and calls it.
//
// Output is deterministic (the accumulator), so the case is golden-checkable cold or warm. Under
// HL_JIT_PCACHE=1 with COLDPROF the cold run logs `deferred-lib=0` (nothing to defer yet) and saves a
// manifest; a warm run logs `deferred-lib=N` then activates all N on the re-map (`warm-note waste=0`).
#include <fcntl.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#if defined(__x86_64__)
// int f(int x /*edi*/) { return x + 7; } :  lea eax,[rdi+7] ; ret
static const unsigned char CODE[] = {0x8D, 0x47, 0x07, 0xC3};
#elif defined(__aarch64__)
// int f(int w0) { return w0 + 7; } :  add w0,w0,#7 ; ret
static const unsigned char CODE[] = {0x00, 0x1C, 0x00, 0x11, 0xC0, 0x03, 0x5F, 0xD6};
#else
#error unsupported guest ISA
#endif

typedef int (*fn_t)(int);

int main(int argc, char **argv) {
    // Per-ISA default filename: the matrix runs the x86 and aarch64 engines in the SAME cwd, so a shared
    // blob name would let one ISA execute the other's machine code (crash). An explicit argv[1] (the
    // policy lane in tests/pcache.rs) overrides this with a private path.
#if defined(__x86_64__)
    const char *path = argc > 1 ? argv[1] : "pclib_blob_x86.bin";
#else
    const char *path = argc > 1 ? argv[1] : "pclib_blob_arm.bin";
#endif
    int fd = open(path, O_RDWR | O_CREAT, 0644); // NO O_TRUNC: keep a stable file identity across runs
    if (fd < 0) {
        printf("pcache libmap open FAILED\n");
        return 2;
    }
    // Write the page exactly once (first cold run). Later runs find the right size and leave mtime/ino
    // untouched, so the file identity the manifest recorded still matches -> deferred blocks activate.
    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_size != 4096) {
        unsigned char page[4096];
        memset(page, 0xC3, sizeof page); // fill with a lone `ret`/`0xC3` pad (harmless)
        memcpy(page, CODE, sizeof CODE);
        if (write(fd, page, sizeof page) != (ssize_t)sizeof page) {
            printf("pcache libmap write FAILED\n");
            return 2;
        }
    }
    // File-backed, non-fixed, executable map: the #178 deterministic-lib-hint path (mem.c hook).
    void *m = mmap(NULL, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE, fd, 0);
    if (m == MAP_FAILED) {
        printf("pcache libmap mmap FAILED\n");
        return 2;
    }
    fn_t f = (fn_t)m;
    long acc = 0;
    for (int i = 0; i < 1000; i++) acc += f(i); // execute code from the library-like map
    munmap(m, 4096);
    close(fd);
    printf("pcache libmap acc=%ld\n", acc); // 506500 = sum(i=0..999) + 7*1000
    return 0;
}
