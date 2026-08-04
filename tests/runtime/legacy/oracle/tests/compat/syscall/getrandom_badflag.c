// CANDIDATE ENGINE BUG: getrandom does not validate its flags argument.
// Native aarch64 Linux: getrandom(buf, n, 0x10) -> -1/EINVAL(22) (0x10 is not GRND_NONBLOCK|GRND_RANDOM|GRND_INSECURE).
// Engine: returns n (success), errno unset.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/random.h>
int main(void){
    unsigned char a[32];
    printf("badflag_errno=%d\n", getrandom(a, sizeof(a), 0x10) == -1 ? errno : 0); // native 22, engine 0
    return 0;
}
