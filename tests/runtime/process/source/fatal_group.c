#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static int blocked[2];
static int ready[2];

static void *blocker(void *unused) {
    char byte;
    ssize_t received;
    (void)unused;
    if (write(ready[1], "r", 1) != 1) _exit(90);
    received = read(blocked[0], &byte, 1);
    _exit(received == 1 ? 91 : 95);
}

static void reraise(int signal_number) {
    signal(signal_number, SIG_DFL);
    raise(signal_number);
}

static void *fault(void *unused) {
    (void)unused;
    signal(SIGSEGV, reraise);
    *(volatile int *)0 = 1;
    _exit(92);
}

int main(void) {
    int survivor_go[2], survivor_ack[2];
    if (pipe(blocked) || pipe(ready) || pipe(survivor_go) || pipe(survivor_ack)) return 1;
    alarm(10);

    pid_t survivor = fork();
    if (survivor == 0) {
        char byte;
        if (read(survivor_go[0], &byte, 1) != 1 || write(survivor_ack[1], "a", 1) != 1) _exit(93);
        _exit(0);
    }
    pid_t victim = fork();
    if (victim == 0) {
        pthread_t blocked_thread, fault_thread;
        char byte;
        if (pthread_create(&blocked_thread, NULL, blocker, NULL) != 0 || read(ready[0], &byte, 1) != 1 ||
            pthread_create(&fault_thread, NULL, fault, NULL) != 0)
            _exit(94);
        for (;;) pause();
    }

    int victim_status = 0, survivor_status = 0;
    char byte;
    int group = waitpid(victim, &victim_status, 0) == victim && WIFSIGNALED(victim_status) &&
                WTERMSIG(victim_status) == SIGSEGV;
    int alive = write(survivor_go[1], "g", 1) == 1 && read(survivor_ack[0], &byte, 1) == 1 &&
                waitpid(survivor, &survivor_status, 0) == survivor && WIFEXITED(survivor_status) &&
                WEXITSTATUS(survivor_status) == 0;
    printf("fatal-group signal=%d survivor=%d\n", group, alive);
    return group && alive ? 0 : 1;
}
