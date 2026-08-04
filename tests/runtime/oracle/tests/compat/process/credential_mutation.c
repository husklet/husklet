#define _GNU_SOURCE
#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <sys/fsuid.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int ids_are(uid_t r, uid_t e, uid_t s, gid_t gr, gid_t ge, gid_t gs) {
    uid_t ur, ue, us;
    gid_t rr, re, rs;
    return getresuid(&ur, &ue, &us) == 0 && getresgid(&rr, &re, &rs) == 0
        && ur == r && ue == e && us == s && rr == gr && re == ge && rs == gs;
}

int main(void) {
    int ok = ids_are(0, 0, 0, 0, 0, 0);
    gid_t wanted[2] = {7, 8}, got[2] = {0, 0};
    ok &= setgroups(2, wanted) == 0 && getgroups(2, got) == 2
        && got[0] == 7 && got[1] == 8;
    ok &= setfsuid(9) == 0 && setfsuid((uid_t)-1) == 9;
    ok &= setfsgid(10) == 0 && setfsgid((gid_t)-1) == 10;
    ok &= setresgid(30, 31, 32) == 0;
    ok &= setresuid(20, 21, 22) == 0;
    ok &= ids_are(20, 21, 22, 30, 31, 32);
    ok &= setfsuid((uid_t)-1) == 21 && setfsgid((gid_t)-1) == 31;

    pid_t child = fork();
    if (child == 0)
        _exit(ids_are(20, 21, 22, 30, 31, 32) ? 0 : 1);
    int status = 0;
    ok &= child > 0 && waitpid(child, &status, 0) == child
        && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    errno = 0;
    ok &= setuid(0) == -1 && errno == EPERM;
    printf("credential-mutation ok=%d\n", ok);
    return ok ? 0 : 1;
}
