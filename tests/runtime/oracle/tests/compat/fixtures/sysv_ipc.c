#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

union hl_semun {
    int val;
    struct semid_ds *buf;
    unsigned short *array;
};

struct test_message {
    long type;
    char text[16];
};

static void fail(const char *what) {
    fprintf(stderr, "sysv_ipc: %s: %s\n", what, strerror(errno));
    exit(1);
}

static void wait_ok(pid_t pid, const char *what) {
    int status = 0;
    if (waitpid(pid, &status, 0) != pid) fail("waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "sysv_ipc: %s child status=%d\n", what, status);
        exit(1);
    }
}

static void test_shared_memory(void) {
    int id = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    if (id < 0) fail("shmget");
    int *value = shmat(id, NULL, 0);
    if (value == (void *)-1) fail("parent shmat");
    *value = 7;

    pid_t pid = fork();
    if (pid < 0) fail("shm fork");
    if (pid == 0) {
        int *child = shmat(id, NULL, 0);
        if (child == (void *)-1 || *child != 7) _exit(2);
        *child = 42;
        if (shmdt(child) != 0) _exit(3);
        _exit(0);
    }
    wait_ok(pid, "shared-memory");
    if (*value != 42) {
        fprintf(stderr, "sysv_ipc: shared-memory update was not visible\n");
        exit(1);
    }
    if (shmdt(value) != 0) fail("parent shmdt");
    if (shmctl(id, IPC_RMID, NULL) != 0) fail("shmctl IPC_RMID");
    errno = 0;
    if (shmat(id, NULL, 0) != (void *)-1 || (errno != EINVAL && errno != EIDRM)) {
        fprintf(stderr, "sysv_ipc: removed shared memory remained attachable errno=%d\n", errno);
        exit(1);
    }
}

static void test_semaphore(void) {
    int id = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    if (id < 0) fail("semget");
    union hl_semun arg;
    arg.val = 0;
    if (semctl(id, 0, SETVAL, arg) != 0) fail("semctl SETVAL");
    int ready[2];
    if (pipe(ready) != 0) fail("sem pipe");

    pid_t pid = fork();
    if (pid < 0) fail("sem fork");
    if (pid == 0) {
        close(ready[0]);
        char byte = 'R';
        if (write(ready[1], &byte, 1) != 1) _exit(2);
        close(ready[1]);
        struct sembuf take = {0, -1, 0};
        if (semop(id, &take, 1) != 0) _exit(3);
        _exit(0);
    }
    close(ready[1]);
    char byte = 0;
    if (read(ready[0], &byte, 1) != 1 || byte != 'R') fail("sem readiness");
    close(ready[0]);
    struct sembuf give = {0, 1, 0};
    if (semop(id, &give, 1) != 0) fail("semop release");
    wait_ok(pid, "semaphore");
    if (semctl(id, 0, IPC_RMID, arg) != 0) fail("semctl IPC_RMID");
    errno = 0;
    if (semop(id, &give, 1) != -1 || (errno != EINVAL && errno != EIDRM)) {
        fprintf(stderr, "sysv_ipc: removed semaphore remained usable errno=%d\n", errno);
        exit(1);
    }
}

static void test_messages(void) {
    int id = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    if (id < 0) fail("msgget");
    struct test_message first = {3, "first"};
    struct test_message second = {3, "second"};
    if (msgsnd(id, &first, sizeof first.text, 0) != 0) fail("msgsnd first");
    if (msgsnd(id, &second, sizeof second.text, 0) != 0) fail("msgsnd second");
    struct test_message got;
    memset(&got, 0, sizeof got);
    if (msgrcv(id, &got, sizeof got.text, 3, 0) != (ssize_t)sizeof got.text || strcmp(got.text, "first") != 0)
        fail("msgrcv first ordering");
    memset(&got, 0, sizeof got);
    if (msgrcv(id, &got, sizeof got.text, 3, 0) != (ssize_t)sizeof got.text || strcmp(got.text, "second") != 0)
        fail("msgrcv second ordering");
    if (msgctl(id, IPC_RMID, NULL) != 0) fail("msgctl IPC_RMID");
    errno = 0;
    if (msgsnd(id, &first, sizeof first.text, IPC_NOWAIT) != -1 || (errno != EINVAL && errno != EIDRM)) {
        fprintf(stderr, "sysv_ipc: removed message queue remained usable errno=%d\n", errno);
        exit(1);
    }
}

int main(void) {
    test_shared_memory();
    test_semaphore();
    test_messages();
    puts("sysv_ipc shm=ok sem=ok msg=ok");
    return 0;
}
