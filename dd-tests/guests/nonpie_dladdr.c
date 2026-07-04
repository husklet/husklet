// #281 regression: a DYNAMIC non-PIE (ET_EXEC) exe's runtime symbol/address introspection must work.
//
// The engine maps a non-PIE image HIGH (+bias) but keeps every guest-visible address at its LOW link
// value (baked &func pointers, un-biased `bl` return addresses; the dispatcher re-biases only at
// execution). The auxv AT_PHDR/AT_ENTRY must therefore also be LOW, so glibc builds the main link_map
// with l_addr==0 and LOW [l_map_start,l_map_end). A HIGH AT_PHDR made glibc set l_addr==bias and HIGH
// ranges; then _dl_find_dso_for_object / dladdr / dlsym(RTLD_NEXT) compared a LOW query against HIGH
// ranges and MISSED -> dladdr()==0, dlsym(RTLD_NEXT,...)==NULL. clickhouse's sanitizer dl_iterate_phdr
// interceptor resolves the real fn via dlsym(RTLD_NEXT,"dl_iterate_phdr"); the NULL made it throw, and
// throwing captured a StackTrace that unwound through the same interceptor -> unbounded recursion ->
// guest stack overflow. This exercises the exact runtime paths, oracle-diffed against native.
//
// Built dynamic `-no-pie -rdynamic` (needs ld.so — hence a rootfs) so the loader link_map path runs.
#define _GNU_SOURCE
#include <stdio.h>
#include <dlfcn.h>

// exported so dladdr can name it and RTLD_DEFAULT can find it
__attribute__((visibility("default"))) int probe_marker(int x) { return x + 1; }

int main(void) {
    Dl_info info;
    // dladdr on a HIGH-executing but LOW-valued image function pointer must resolve to this object.
    int da = dladdr((void *)&probe_marker, &info);
    int has_name = da && info.dli_sname && !__builtin_strcmp(info.dli_sname, "probe_marker");

    // dlsym(RTLD_NEXT, ...) uses the CALLER's (un-biased) return address to find the calling object, then
    // searches the objects after it — the exact lookup that returned NULL when the ranges were HIGH.
    void *next_malloc = dlsym(RTLD_NEXT, "malloc");
    void *def_self    = dlsym(RTLD_DEFAULT, "probe_marker");

    printf("dladdr=%d sname_ok=%d rtld_next_malloc=%d rtld_default_self=%d\n",
           da ? 1 : 0, has_name ? 1 : 0, next_malloc ? 1 : 0, def_self ? 1 : 0);
    return 0;
}
