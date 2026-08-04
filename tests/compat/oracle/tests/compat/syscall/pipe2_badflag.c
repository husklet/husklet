// CANDIDATE ENGINE BUG: pipe2 does not validate its flags argument.
// Native aarch64 Linux: pipe2(pf, 0x4) -> -1/EINVAL(22). Engine: returns 0 (success), errno unset.
// eventfd2 with the same bogus flag IS correctly rejected by the engine, so the validation gap is pipe2-specific.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
int main(void){
    int pf[2];
    printf("pipe2_badflag_errno=%d\n", pipe2(pf, 0x4) == -1 ? errno : 0);   // native 22, engine 0
    printf("eventfd2_badflag_errno=%d\n", -1); // context only
    return 0;
}
