// EDGE: /proc/self/fd — the live fd table as symlinks. Open a known file, then readlink
// /proc/self/fd/<n> and confirm it resolves back to that file; also count entries in /proc/self/fd.
// Many runtimes (and tools like lsof, and glibc's closefrom) rely on it. Verdict-checked (the exact
// path/dirfd numbers vary) so it's golden across engines.
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    const char *path = "/tmp/hl_procfd_target";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    char link[64], target[256] = {0};
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, target, sizeof target - 1);
    int resolves = (n > 0) && (strstr(target, "hl_procfd_target") != NULL);

    DIR *d = opendir("/proc/self/fd");
    int count = 0;
    if (d) { struct dirent *e; while ((e = readdir(d))) if (e->d_name[0] != '.') count++; closedir(d); }
    int pipefd[2] = {-1, -1};
    int devfd_pipe = 0, devfd_metadata = 0;
    if (pipe(pipefd) == 0 && write(pipefd[1], "procfd", 6) == 6) {
        char devfd[64], bytes[7] = {0}, link_text[128];
        snprintf(devfd, sizeof devfd, "/dev/fd/%d", pipefd[0]);
        struct stat link_stat = {0}, target_stat = {0};
        int dl = lstat(devfd, &link_stat) == 0 && S_ISLNK(link_stat.st_mode);
        int ds = stat(devfd, &target_stat) == 0 && S_ISFIFO(target_stat.st_mode);
        int da = access(devfd, R_OK) == 0;
        int dr = readlink(devfd, link_text, sizeof link_text) > 0;
        devfd_metadata = dl && ds && da && dr;
        printf("devfd-parts=%d%d%d%d\n", dl, ds, da, dr);
        int reopened = open(devfd, O_RDONLY | O_CLOEXEC);
        devfd_pipe = reopened >= 0 && read(reopened, bytes, 6) == 6 && strcmp(bytes, "procfd") == 0;
        if (reopened >= 0) close(reopened);
        close(pipefd[0]);
        close(pipefd[1]);
    }
    close(fd);
    unlink(path);
    // at least stdin/out/err + our fd + the DIR fd are open
    printf("procfd resolves=%d enough_fds=%d devfd-metadata=%d devfd-pipe=%d\n", resolves, count >= 4,
           devfd_metadata, devfd_pipe); // 1 1 1 1
    return 0;
}
