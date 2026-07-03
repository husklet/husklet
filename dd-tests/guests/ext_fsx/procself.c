// /proc/self/root and /proc/self/cwd magic symlinks (Linux): readlink(/proc/self/root) -> the process
// root ("/"), readlink(/proc/self/cwd) -> the current working directory (an absolute path). Go/Rust path
// code and some container init resolve these. Linux-only: macOS has no /proc.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char r[256] = {0};
    ssize_t rn = readlink("/proc/self/root", r, sizeof r - 1);
    int root_ok = rn > 0 && strcmp(r, "/") == 0;
    char cwd[256] = {0};
    ssize_t cn = readlink("/proc/self/cwd", cwd, sizeof cwd - 1);
    int cwd_abs = cn > 0 && cwd[0] == '/';
    printf("procself root=%d cwd=%d\n", root_ok, cwd_abs);
    return 0;
}
