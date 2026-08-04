// CANDIDATE ENGINE BUG: clock_gettime and clock_adjtime with a garbage clock id must fail with
// EINVAL. The engine returns 0 (success) for both, silently accepting the unknown id.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/timex.h>
#include <time.h>
int main(void){
    struct timespec ts; errno=0;
    int get_bad = clock_gettime((clockid_t)0x7fff,&ts)==-1 && errno==EINVAL;
    struct timex tx; memset(&tx,0,sizeof tx); errno=0;
    int adj_bad = clock_adjtime((clockid_t)0x7fff,&tx)==-1 && errno==EINVAL;
    printf("clockidval get_bad=%d adj_bad=%d\n", get_bad, adj_bad);
    return 0;
}
