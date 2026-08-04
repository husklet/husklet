// CANDIDATE ENGINE BUG: RLIMIT_NOFILE soft limit is not enforced on open().
// Native aarch64 Linux: after setrlimit(RLIMIT_NOFILE, {cur=64}), opening fds past the limit -> EMFILE(24).
// Engine: getrlimit reads back 64 (soft_set=1) and raising above the hard limit is EPERM(1), but open()
// keeps succeeding past the soft limit, so no EMFILE is ever produced (emfile_errno stays 0).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/resource.h>
#include <unistd.h>
int main(void){
    struct rlimit rl; getrlimit(RLIMIT_NOFILE,&rl); rl.rlim_cur=64; setrlimit(RLIMIT_NOFILE,&rl);
    int last=0;
    for(int i=0;i<200;i++){ int fd=open("/dev/null",O_RDONLY); if(fd==-1){last=errno;break;} }
    printf("emfile_errno=%d\n", last); // native 24, engine 0
    return 0;
}
