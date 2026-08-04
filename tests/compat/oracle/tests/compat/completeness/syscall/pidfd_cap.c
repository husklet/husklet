#include "compat.h"
#include <stdio.h>
#include <errno.h>

/* Open many simultaneous pidfds. The invariant Linux guarantees (and hl must too): every SUCCESSFUL
   pidfd_open returns a fd that stays resolvable (pidfd_send_signal(fd,0) works); a failure is a clean errno,
   never a usable-looking fd that later can't be resolved. hl's fixed 64-slot table used to silently hand
   back an unresolvable fd past capacity -- now it fails EMFILE. Print the invariant boolean, oracle-diffed. */

#define N 96

int main(void) {
  int fds[N];
  int invariant = 1;
  for (int i = 0; i < N; i++) {
    long fd = syscall(__NR_pidfd_open, (long)getpid(), 0L);
    fds[i] = (int)fd;
    if (fd >= 0) {
      /* a returned fd MUST be resolvable */
      long r = syscall(__NR_pidfd_send_signal, fd, 0L, (void *)0, 0L);
      if (r != 0) invariant = 0;
    } else {
      /* a failure must be a clean resource errno, not garbage */
      if (!(errno == EMFILE || errno == ENFILE || errno == ENOMEM || errno == EMFILE)) invariant = 0;
    }
  }
  printf("pidfd_cap invariant=%d\n", invariant);
  return 0;
}
