#define _GNU_SOURCE

#if defined(__linux__)
#include <stddef.h>
#include <sys/stat.h>

#if defined(__aarch64__)
_Static_assert(sizeof(struct stat) == 128, "Linux AArch64 stat size");
_Static_assert(offsetof(struct stat, st_mode) == 16, "Linux AArch64 mode offset");
_Static_assert(offsetof(struct stat, st_size) == 48, "Linux AArch64 size offset");
_Static_assert(offsetof(struct stat, st_blocks) == 64, "Linux AArch64 blocks offset");
_Static_assert(offsetof(struct stat, st_mtim) == 88, "Linux AArch64 mtime offset");
#elif defined(__x86_64__)
_Static_assert(sizeof(struct stat) == 144, "Linux x86_64 stat size");
_Static_assert(offsetof(struct stat, st_mode) == 24, "Linux x86_64 mode offset");
_Static_assert(offsetof(struct stat, st_size) == 48, "Linux x86_64 size offset");
_Static_assert(offsetof(struct stat, st_blocks) == 64, "Linux x86_64 blocks offset");
_Static_assert(offsetof(struct stat, st_mtim) == 88, "Linux x86_64 mtime offset");
#endif
#endif

int main(void) {
    return 0;
}
