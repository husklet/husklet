// translit/executable_mapping -- the refusal that costs the most, and the one nothing announced.
//
// g_rwx_guest latches on the first ANONYMOUS PROT_EXEC mmap (or an mprotect that adds PROT_EXEC), and
// jit86_store_alias_observation_active() then makes translit_image_ok() false for the rest of the
// process. That is deliberate -- the transliterator caches guest BYTES and a verbatim store neither
// queues writeback for an emulated MAP_SHARED alias nor arms SMC page protection -- but the latch is
// NOT cleared by execve, so one 4 KiB page of JIT arena disables the backend for this process and for
// every image it goes on to exec. Measured on this host: the identical compute loop runs 8.77x fewer
// host instructions with the backend selected, and exactly 1.000x when reached through an execve that
// followed a single anonymous PROT_EXEC mmap.
//
// Every JIT-hosting guest is in that state within milliseconds of starting -- a JVM, V8, .NET, LuaJIT --
// so this fixture is what stops "the transliterator is 2-5x" from being said about them.
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>

static void work(void) {
    unsigned long long h = 1469598103934665603ull;
    for (unsigned long long i = 0; i < 2000000ull; i++) {
        h ^= i;
        h *= 1099511628211ull;
        h += h >> 7;
    }
    printf("work h=%016llx\n", h);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc > 1 && !strcmp(argv[1], "execed")) {
        work();
        return 0;
    }
    work();                                                  // still eligible here
    if (mmap(NULL, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, // the latch, and it is permanent
             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    work();                                          // refused from here on
    execl(argv[0], argv[0], "execed", (char *)NULL); // and the refusal survives this
    return 42;
}
