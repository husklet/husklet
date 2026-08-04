// POSIX named semaphores: sem_open create/reopen, post/wait/trywait, getvalue, unlink.
#include <semaphore.h>
#include <fcntl.h>
#include <stdio.h>
#include <errno.h>
#include <unistd.h>

int main(void) {
    char name[64];
    snprintf(name, sizeof name, "/hl_sem_%d", (int)getpid());
    sem_unlink(name);
    sem_t *s = sem_open(name, O_CREAT | O_EXCL, 0600, 1);
    if (s == SEM_FAILED) { printf("semnamed open=0\n"); return 0; }

    // O_EXCL on existing name fails with EEXIST.
    sem_t *dup = sem_open(name, O_CREAT | O_EXCL, 0600, 1);
    int excl = dup == SEM_FAILED && errno == EEXIST;

    int v0 = -1;
    sem_getvalue(s, &v0);
    int w = sem_wait(s) == 0;
    int v1 = -1;
    sem_getvalue(s, &v1);
    int tw = sem_trywait(s) < 0 && errno == EAGAIN; // now 0, trywait fails
    sem_post(s);
    int v2 = -1;
    sem_getvalue(s, &v2);

    sem_close(s);
    int unlinked = sem_unlink(name) == 0;
    printf("semnamed excl=%d v0=%d wait=%d v1=%d empty_eagain=%d v2=%d unlink=%d\n",
           excl, v0 == 1, w, v1 == 0, tw, v2 == 1, unlinked);
    return 0;
}
