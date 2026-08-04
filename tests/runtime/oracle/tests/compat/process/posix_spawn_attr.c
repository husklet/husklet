// posix_spawn with a spawnattr: POSIX_SPAWN_SETPGROUP puts the spawned child into its own new process
// group (pgid==its own pid), and POSIX_SPAWN_SETSIGDEF resets a signal the parent had ignored back to
// default in the child. The child re-execs this binary in "child" mode and reports what it observed.
#define _GNU_SOURCE
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

// child: report (own pgid == own pid) as bit0, (SIGTERM disposition == DFL) as bit1.
static int child_main(void) {
    int own_group = getpgid(0) == getpid();
    struct sigaction sa;
    sigaction(SIGTERM, NULL, &sa);
    int term_dfl = sa.sa_handler == SIG_DFL;
    return (own_group ? 1 : 0) | (term_dfl ? 2 : 0);
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "child") == 0) return child_main();

    signal(SIGTERM, SIG_IGN); // parent ignores SIGTERM; SETSIGDEF must reset it in the child

    posix_spawnattr_t attr;
    posix_spawnattr_init(&attr);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGDEF);
    posix_spawnattr_setpgroup(&attr, 0); // 0 -> child becomes leader of its own new group
    sigset_t def;
    sigemptyset(&def);
    sigaddset(&def, SIGTERM);
    posix_spawnattr_setsigdefault(&attr, &def);

    char *cargv[] = { argv[0], (char *)"child", NULL };
    pid_t pid = 0;
    int rc = posix_spawn(&pid, argv[0], NULL, &attr, cargv, environ);
    int spawned = rc == 0 && pid > 0;
    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    posix_spawnattr_destroy(&attr);

    printf("posix_spawn_attr spawned=%d own_group=%d term_dfl=%d\n",
           spawned, (code & 1) ? 1 : 0, (code & 2) ? 1 : 0);
    return 0;
}
