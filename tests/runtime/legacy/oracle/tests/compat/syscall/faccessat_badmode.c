// CANDIDATE ENGINE BUG: faccessat does not validate its mode argument.
// Native aarch64 Linux: faccessat(AT_FDCWD, path, 0x8, 0) -> -1/EINVAL(22) (0x8 is outside R_OK|W_OK|X_OK|F_OK).
// Engine: returns 0 (success), errno unset.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
int main(void){
    printf("badmode_errno=%d\n", faccessat(AT_FDCWD, "/", 0x8, 0) == -1 ? errno : 0); // native 22, engine 0
    return 0;
}
