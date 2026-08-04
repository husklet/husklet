#include <errno.h>
#include <stdio.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <sys/wait.h>
#include <unistd.h>

union semun {
    int val;
    struct semid_ds *buf;
    unsigned short *array;
};

struct message {
    long type;
    char value;
};

int main(void) {
    int shmid = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    int semid = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    int msgid = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    int *shared = shmid < 0 ? (void *)-1 : shmat(shmid, NULL, 0);
    union semun argument = {.val = 1};
    if (shmid < 0 || semid < 0 || msgid < 0 || shared == (void *)-1 ||
        semctl(semid, 0, SETVAL, argument) != 0) return 20;
    *shared = 7;

    pid_t child = fork();
    if (child < 0) return 21;
    if (child == 0) {
        struct sembuf take = {.sem_num = 0, .sem_op = -1, .sem_flg = SEM_UNDO};
        struct message message = {.type = 1, .value = 42};
        if (*shared != 7 || semop(semid, &take, 1) != 0) _exit(30);
        *shared = 19;
        if (msgsnd(msgid, &message, sizeof(message.value), 0) != 0) _exit(31);
        if (shmctl(shmid, IPC_RMID, NULL) != 0) _exit(32);
        _exit(0);
    }

    int status = 0;
    int child_ok = waitpid(child, &status, 0) == child &&
        WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int shared_ok = *shared == 19;
    int undo_ok = semctl(semid, 0, GETVAL) == 1;
    struct message message = {0};
    int message_ok = msgrcv(msgid, &message, sizeof(message.value), 1, 0) == 1 &&
        message.value == 42;
    int detached = shmdt(shared) == 0;
    errno = 0;
    struct shmid_ds metadata;
    int removed = detached && shmctl(shmid, IPC_STAT, &metadata) == -1 &&
        (errno == EINVAL || errno == EIDRM);
    int cleaned = semctl(semid, 0, IPC_RMID) == 0 && msgctl(msgid, IPC_RMID, NULL) == 0;

    printf("sysv_fork shared=%d undo=%d message=%d removed=%d child=%d cleaned=%d\n",
        shared_ok, undo_ok, message_ok, removed, child_ok, cleaned);
    return shared_ok && undo_ok && message_ok && removed && child_ok && cleaned ? 0 : 1;
}
