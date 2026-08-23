#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t died;

static void death(int signal) {
    died = signal == SIGUSR1;
}

static int parent_death(void) {
    int ready[2], result[2];
    if (pipe(ready) || pipe(result)) return 0;
    pid_t middle = fork();
    if (middle == 0) {
        pid_t leaf = fork();
        if (leaf == 0) {
            close(ready[0]);
            close(result[0]);
            struct sigaction action = {.sa_handler = death};
            sigemptyset(&action.sa_mask);
            if (sigaction(SIGUSR1, &action, 0) || prctl(PR_SET_PDEATHSIG, SIGUSR1, 0, 0, 0)) _exit(3);
            char byte = 'R';
            if (write(ready[1], &byte, 1) != 1) _exit(4);
            while (!died)
                pause();
            byte = died ? 'D' : 'F';
            write(result[1], &byte, 1);
            _exit(died ? 0 : 5);
        }
        close(ready[1]);
        close(result[0]);
        close(result[1]);
        char byte;
        if (read(ready[0], &byte, 1) != 1) _exit(6);
        _exit(0);
    }
    close(ready[0]);
    close(ready[1]);
    close(result[1]);
    int status = 0;
    char byte = 0;
    int ok = middle > 0 && waitpid(middle, &status, 0) == middle && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    ok &= read(result[0], &byte, 1) == 1 && byte == 'D';
    return ok;
}

static int subreaper(void) {
    if (prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0)) return 0;
    int value = 0;
    if (prctl(PR_GET_CHILD_SUBREAPER, &value, 0, 0, 0) || value != 1) return 0;
    if (prctl(PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0)) return 0;
    value = 1;
    return prctl(PR_GET_CHILD_SUBREAPER, &value, 0, 0, 0) == 0 && value == 0;
}

int main(void) {
    int parent = parent_death();
    int sub = subreaper();
    int ok = parent && sub;
    printf("prctl-lifecycle ok=%d\n", ok);
    return ok ? 0 : 1;
}
