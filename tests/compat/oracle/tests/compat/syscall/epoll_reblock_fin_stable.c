#define _GNU_SOURCE
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

static int stable_printf(const char *format, ...) {
    va_list arguments;
    int result;
    if (strcmp(format, "FINITE_OK ms=%ld\n") == 0) return printf("FINITE_OK\n");
    va_start(arguments, format);
    result = vprintf(format, arguments);
    va_end(arguments);
    return result;
}

#define main legacy_epoll_reblock_fin_main
#define printf stable_printf
#include "epoll_reblock_fin.c"
#undef printf
#undef main

int main(void) { return legacy_epoll_reblock_fin_main(); }
