// whichfunc.c — print WHICH glibc ifunc implementation got selected for the hot string/memory
// functions, by matching the resolved function pointer against glibc's own implementation list
// (__libc_ifunc_impl_list, the mechanism behind glibc's test-multiarch). Static-link friendly.
//
// This makes the CPUID-model / ERMS / FSRM effect on glibc's ifunc selection directly observable
// per engine build / GLIBC_TUNABLES config, instead of inferring it from timings.
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

struct libc_ifunc_impl {
    const char *name;
    void (*fn)(void);
    bool usable;
};
extern size_t __libc_ifunc_impl_list(const char *name, struct libc_ifunc_impl *array, size_t max);

static void which(const char *name, void *resolved) {
    struct libc_ifunc_impl impls[64];
    size_t n = __libc_ifunc_impl_list(name, impls, 64);
    const char *sel = "(no ifunc list entry)";
    for (size_t i = 0; i < n; i++)
        if ((void *)impls[i].fn == resolved) { sel = impls[i].name; break; }
    printf("%-10s -> %s\n", name, sel);
}

// volatile pointers so the compiler can't substitute its own builtins for the address-of
static void *(*volatile p_memcpy)(void *, const void *, size_t) = memcpy;
static void *(*volatile p_memmove)(void *, const void *, size_t) = memmove;
static void *(*volatile p_memset)(void *, int, size_t) = memset;
static size_t (*volatile p_strlen)(const char *) = strlen;
static int (*volatile p_strcmp)(const char *, const char *) = strcmp;
static int (*volatile p_memcmp)(const void *, const void *, size_t) = memcmp;
static void *(*volatile p_memchr)(const void *, int, size_t) = memchr;
static char *(*volatile p_strchr)(const char *, int) = strchr;
static int (*volatile p_strncmp)(const char *, const char *, size_t) = strncmp;
static char *(*volatile p_strcpy)(char *, const char *) = strcpy;
static char *(*volatile p_strstr)(const char *, const char *) = strstr;

int main(void) {
    which("memcpy", (void *)p_memcpy);
    which("memmove", (void *)p_memmove);
    which("memset", (void *)p_memset);
    which("strlen", (void *)p_strlen);
    which("strcmp", (void *)p_strcmp);
    which("memcmp", (void *)p_memcmp);
    which("memchr", (void *)p_memchr);
    which("strchr", (void *)p_strchr);
    which("strncmp", (void *)p_strncmp);
    which("strcpy", (void *)p_strcpy);
    which("strstr", (void *)p_strstr);
    return 0;
}
