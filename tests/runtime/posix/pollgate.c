/* poll/ppoll/select semantics, pinned against the Linux oracle.
 *
 * These are the cases the Darwin select(2) gate in linux_abi/syscall/event/poll.c must preserve: an idle
 * scan, an immediately ready scan, a mixed scan, a negative descriptor, an events==0 request, a timeout
 * that actually elapses, ppoll's timespec form, a descriptor above FD_SETSIZE, POLLNVAL from a closed
 * descriptor alone and beside a live one, POLLHUP after the writer closes, and EINTR out of a blocking
 * wait -- plus the select(2) mirrors of the idle and ready scans. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <time.h>
#include <unistd.h>

static void ok(const char *n, int rc, short rev) { printf("%-28s rc=%d revents=%#x\n", n, rc, rev); }
static long long ms_now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return (long long)t.tv_sec*1000+t.tv_nsec/1000000;}
static volatile int got;
static void onsig(int s){(void)s;got=1;}

int main(void) {
    int p[2];
    struct pollfd f[3];
    int rc;

    /* 1. nfds=0 */
    rc = poll(NULL, 0, 0);
    printf("%-28s rc=%d\n", "nfds0_t0", rc);
    { long long a=ms_now(); rc=poll(NULL,0,60); long long d=ms_now()-a;
      printf("%-28s rc=%d slept=%d\n","nfds0_t60",rc,d>=55); }

    if (pipe(p)) return 1;

    /* 2. idle read end, timeout 0 */
    f[0].fd=p[0]; f[0].events=POLLIN; f[0].revents=0x5a5a;
    rc=poll(f,1,0); ok("idle_pollin_t0",rc,f[0].revents);

    /* 3. write end is writable */
    f[0].fd=p[1]; f[0].events=POLLOUT; f[0].revents=0;
    rc=poll(f,1,0); ok("writeend_pollout_t0",rc,f[0].revents);

    /* 4. data present */
    (void)!write(p[1],"x",1);
    f[0].fd=p[0]; f[0].events=POLLIN; f[0].revents=0;
    rc=poll(f,1,0); ok("ready_pollin_t0",rc,f[0].revents);
    f[0].revents=0; rc=poll(f,1,-1); ok("ready_pollin_tneg",rc,f[0].revents);

    /* 5. mixed: ready + idle, two entries */
    f[0].fd=p[0]; f[0].events=POLLIN; f[0].revents=0;
    f[1].fd=p[1]; f[1].events=POLLIN; f[1].revents=0;
    rc=poll(f,2,0); printf("%-28s rc=%d r0=%#x r1=%#x\n","mixed_two",rc,f[0].revents,f[1].revents);
    { char b; (void)!read(p[0],&b,1); }

    /* 6. negative fd is skipped */
    f[0].fd=-1; f[0].events=POLLIN; f[0].revents=0x5a5a;
    rc=poll(f,1,0); ok("negative_fd",rc,f[0].revents);

    /* 7. events==0 on an idle fd */
    f[0].fd=p[0]; f[0].events=0; f[0].revents=0x5a5a;
    rc=poll(f,1,0); ok("events0_idle",rc,f[0].revents);

    /* 8. timeout actually elapses */
    f[0].fd=p[0]; f[0].events=POLLIN; f[0].revents=0;
    { long long a=ms_now(); rc=poll(f,1,60); long long d=ms_now()-a;
      printf("%-28s rc=%d revents=%#x slept=%d\n","idle_timeout60",rc,f[0].revents,d>=55); }

    /* 9. ppoll with a timespec */
    { struct timespec ts={0,0}; f[0].fd=p[0]; f[0].events=POLLIN; f[0].revents=0x5a5a;
      rc=ppoll(f,1,&ts,NULL); ok("ppoll_idle_zero",rc,f[0].revents); }
    { struct timespec ts={0,0}; f[0].fd=p[1]; f[0].events=POLLOUT; f[0].revents=0;
      rc=ppoll(f,1,&ts,NULL); ok("ppoll_ready_zero",rc,f[0].revents); }

    /* 10. a descriptor above FD_SETSIZE (the select gate must decline and still be correct) */
    { int hi=fcntl(p[0],F_DUPFD,2000);
      f[0].fd=hi; f[0].events=POLLIN; f[0].revents=0x5a5a;
      rc=poll(f,1,0); printf("%-28s rc=%d revents=%#x above=%d\n","highfd_idle",rc,f[0].revents,hi>=2000);
      (void)!write(p[1],"y",1);
      f[0].revents=0; rc=poll(f,1,0); ok("highfd_ready",rc,f[0].revents);
      { char b; (void)!read(p[0],&b,1); } close(hi); }

    /* 11. POLLNVAL on a closed descriptor, alone and beside a live one */
    { int c=dup(p[0]); close(c);
      f[0].fd=c; f[0].events=POLLIN; f[0].revents=0;
      rc=poll(f,1,0); ok("closed_fd_alone",rc,f[0].revents);
      f[0].fd=c; f[0].events=POLLIN; f[0].revents=0;
      f[1].fd=p[1]; f[1].events=POLLOUT; f[1].revents=0;
      rc=poll(f,2,0); printf("%-28s rc=%d r0=%#x r1=%#x\n","closed_fd_with_live",rc,f[0].revents,f[1].revents); }

    /* 12. POLLHUP after the writer closes */
    { int q[2]; if(pipe(q)) return 1; close(q[1]);
      f[0].fd=q[0]; f[0].events=POLLIN; f[0].revents=0;
      rc=poll(f,1,0);
      printf("%-28s rc=%d hup=%d\n","hup_pollin",rc,(f[0].revents&POLLHUP)?1:0);
      close(q[0]); }

    /* 13. EINTR out of a blocking poll */
    { struct sigaction sa; memset(&sa,0,sizeof sa); sa.sa_handler=onsig; sigaction(SIGALRM,&sa,NULL);
      got=0; alarm(1);
      f[0].fd=p[0]; f[0].events=POLLIN; f[0].revents=0;
      rc=poll(f,1,5000);
      printf("%-28s rc=%d eintr=%d handled=%d\n","blocking_eintr",rc,rc<0&&errno==EINTR,got);
      alarm(0); }

    /* 14. select mirrors */
    { fd_set r; struct timeval tv={0,0};
      FD_ZERO(&r); FD_SET(p[0],&r); rc=select(p[0]+1,&r,NULL,NULL,&tv);
      printf("%-28s rc=%d set=%d\n","select_idle",rc,FD_ISSET(p[0],&r)?1:0);
      (void)!write(p[1],"z",1);
      FD_ZERO(&r); FD_SET(p[0],&r); tv.tv_sec=0; tv.tv_usec=0; rc=select(p[0]+1,&r,NULL,NULL,&tv);
      printf("%-28s rc=%d set=%d\n","select_ready",rc,FD_ISSET(p[0],&r)?1:0);
      { char b; (void)!read(p[0],&b,1); } }

    printf("done\n");
    return 0;
}
