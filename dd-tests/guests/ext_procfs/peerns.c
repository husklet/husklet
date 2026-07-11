// lsns/nsenter inspect a peer process's namespaces via /proc/<pid>/ns. A container is a single
// namespace set, so a live child's ns dir must stat, its ns/ must list (e.g. "net"), and
// /proc/<child>/ns/net must readlink to "net:[<inode>]" -- the same values self reports. Before the fix
// the peer ns readlink fell through to a host readlink (ENOENT). Verdict ok=1.
#include <dirent.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    pid_t ch = fork();
    if (ch == 0) {
        pause();
        _exit(0);
    }
    usleep(150000); // let the child register in the proc registry
    int ok = 1;
    char path[64];
    struct stat st;
    snprintf(path, sizeof path, "/proc/%d/ns", ch);
    ok &= (stat(path, &st) == 0);
    DIR *d = opendir(path);
    int saw_net = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)))
            if (!strcmp(e->d_name, "net")) saw_net = 1;
        closedir(d);
    }
    ok &= saw_net;
    snprintf(path, sizeof path, "/proc/%d/ns/net", ch);
    char lb[128];
    ssize_t r = readlink(path, lb, sizeof lb - 1);
    ok &= (r > 0);
    if (r > 0) {
        lb[r] = 0;
        ok &= (strncmp(lb, "net:[", 5) == 0);
    }
    kill(ch, SIGKILL);
    waitpid(ch, NULL, 0);
    printf("peer_ns ok=%d\n", ok);
    return 0;
}
