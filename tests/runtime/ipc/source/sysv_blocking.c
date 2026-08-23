#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
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

static volatile sig_atomic_t interrupted;

static void interrupt_wait(int signal) {
    (void)signal;
    interrupted = 1;
}

static void pause_parent(void) {
    struct timespec delay = {.tv_sec = 0, .tv_nsec = 20 * 1000 * 1000};
    nanosleep(&delay, NULL);
}

static int child_ok(pid_t child) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static int message_wake(void) {
    int id = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    pid_t child = fork();
    if (id < 0 || child < 0) return 0;
    if (child == 0) {
        struct message message = {0};
        ssize_t bytes = msgrcv(id, &message, sizeof(message.value), 1, 0);
        _exit(bytes == 1 && message.value == 42 ? 0 : 1);
    }
    pause_parent();
    struct message message = {.type = 1, .value = 42};
    int sent = msgsnd(id, &message, sizeof(message.value), 0) == 0;
    int result = sent && child_ok(child);
    msgctl(id, IPC_RMID, NULL);
    return result;
}

static int message_remove(void) {
    int id = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    pid_t child = fork();
    if (id < 0 || child < 0) return 0;
    if (child == 0) {
        struct message message = {0};
        errno = 0;
        ssize_t bytes = msgrcv(id, &message, sizeof(message.value), 1, 0);
        _exit(bytes == -1 && errno == EIDRM ? 0 : 1);
    }
    pause_parent();
    int removed = msgctl(id, IPC_RMID, NULL) == 0;
    return removed && child_ok(child);
}

static int semaphore_wake(void) {
    int id = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    union semun value = {.val = 0};
    if (id < 0 || semctl(id, 0, SETVAL, value) != 0) return 0;
    pid_t child = fork();
    if (child < 0) return 0;
    if (child == 0) {
        struct sembuf take = {.sem_num = 0, .sem_op = -1, .sem_flg = 0};
        _exit(semop(id, &take, 1) == 0 ? 0 : 1);
    }
    pause_parent();
    struct sembuf post = {.sem_num = 0, .sem_op = 1, .sem_flg = 0};
    int posted = semop(id, &post, 1) == 0;
    int result = posted && child_ok(child);
    semctl(id, 0, IPC_RMID);
    return result;
}

static int semaphore_remove(void) {
    int id = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    if (id < 0) return 0;
    pid_t child = fork();
    if (child < 0) return 0;
    if (child == 0) {
        struct sembuf take = {.sem_num = 0, .sem_op = -1, .sem_flg = 0};
        errno = 0;
        int result = semop(id, &take, 1);
        _exit(result == -1 && errno == EIDRM ? 0 : 1);
    }
    pause_parent();
    int removed = semctl(id, 0, IPC_RMID) == 0;
    return removed && child_ok(child);
}

static int semaphore_timeout(void) {
    int id = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    if (id < 0) return 0;
    struct sembuf take = {.sem_num = 0, .sem_op = -1, .sem_flg = 0};
    struct timespec timeout = {.tv_sec = 0, .tv_nsec = 20 * 1000 * 1000};
    errno = 0;
    int result = semtimedop(id, &take, 1, &timeout);
    int matched = result == -1 && errno == EAGAIN;
    semctl(id, 0, IPC_RMID);
    return matched;
}

static int message_interrupt(void) {
    int id = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    if (id < 0) return 0;
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = interrupt_wait;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, NULL) != 0) return 0;
    struct itimerval timer = {.it_value = {.tv_sec = 0, .tv_usec = 20 * 1000}};
    if (setitimer(ITIMER_REAL, &timer, NULL) != 0) return 0;
    struct message message = {0};
    errno = 0;
    ssize_t result = msgrcv(id, &message, sizeof(message.value), 1, 0);
    int matched = result == -1 && errno == EINTR && interrupted == 1;
    msgctl(id, IPC_RMID, NULL);
    return matched;
}

int main(void) {
    int msg_wake = message_wake();
    int msg_remove = message_remove();
    int sem_wake = semaphore_wake();
    int sem_remove = semaphore_remove();
    int sem_timeout = semaphore_timeout();
    int msg_eintr = message_interrupt();
    printf("sysv_blocking msg_wake=%d msg_remove=%d sem_wake=%d sem_remove=%d timeout=%d eintr=%d\n", msg_wake,
           msg_remove, sem_wake, sem_remove, sem_timeout, msg_eintr);
    return msg_wake && msg_remove && sem_wake && sem_remove && sem_timeout && msg_eintr ? 0 : 1;
}
