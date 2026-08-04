#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <unistd.h>

union semun {
    int val;
    struct semid_ds *buf;
    unsigned short *array;
};

int main(void) {
    if (geteuid() == 0 && (setegid(70) != 0 || seteuid(70) != 0)) {
        perror("drop");
        return 1;
    }

    int shm = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    void *memory = shm < 0 ? (void *)-1 : shmat(shm, NULL, 0);
    struct shmid_ds shmds;
    int shm_ok = memory != (void *)-1 && shmctl(shm, IPC_STAT, &shmds) == 0 &&
                 shmds.shm_perm.uid == geteuid() && shmds.shm_perm.cuid == geteuid();

    int sem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    struct semid_ds semds;
    union semun argument = {.buf = &semds};
    int sem_ok = sem >= 0 && semctl(sem, 0, IPC_STAT, argument) == 0 &&
                 semds.sem_perm.uid == geteuid() && semds.sem_perm.cuid == geteuid();

    int msg = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    struct msqid_ds msgds;
    int msg_ok = msg >= 0 && msgctl(msg, IPC_STAT, &msgds) == 0 &&
                 msgds.msg_perm.uid == geteuid() && msgds.msg_perm.cuid == geteuid();

    printf("owner shm=%d sem=%d msg=%d\n", shm_ok, sem_ok, msg_ok);

    if (memory != (void *)-1) shmdt(memory);
    if (shm >= 0) shmctl(shm, IPC_RMID, NULL);
    if (sem >= 0) semctl(sem, 0, IPC_RMID);
    if (msg >= 0) msgctl(msg, IPC_RMID, NULL);
    return (shm_ok && sem_ok && msg_ok) ? 0 : 1;
}
