// chmod/fchmod set permission bits; guest fchown persists virtual ownership without changing the host owner.
#include <fcntl.h>
#include <linux/stat.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_chmod_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_WRONLY, 0644);
    close(fd);
    int c1 = chmod(path, 0600) == 0;
    struct stat st; stat(path, &st);
    int is600 = (st.st_mode & 0777) == 0600;
    fd = open(path, O_RDONLY);
    int c2 = fchmod(fd, 0755) == 0;
    stat(path, &st);
    int is755 = (st.st_mode & 0777) == 0755;
    int ch = chown(path, getuid(), getgid()) == 0; // self -> allowed
    int fch = fchown(fd, 123, 456) == 0;
    fstat(fd, &st);
    int fstat_owner = st.st_uid == 123 && st.st_gid == 456;
    close(fd);
    fd = open(path, O_RDONLY);
    fstat(fd, &st);
    int reopen_owner = st.st_uid == 123 && st.st_gid == 456;
    char hard[160];
    snprintf(hard, sizeof hard, "%s.link", path);
    int hard_owner = link(path, hard) == 0 && stat(hard, &st) == 0 && st.st_uid == 123 && st.st_gid == 456;
    struct statx sx;
    int statx_owner = syscall(SYS_statx, AT_FDCWD, hard, 0, STATX_UID | STATX_GID, &sx) == 0 &&
                      sx.stx_uid == 123 && sx.stx_gid == 456;
    pid_t child = fork();
    if (child == 0) {
        struct stat cs;
        _exit(stat(hard, &cs) == 0 && cs.st_uid == 123 && cs.st_gid == 456 ? 0 : 1);
    }
    int child_status = 0;
    waitpid(child, &child_status, 0);
    int fork_owner = WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0;
    close(fd);
    unlink(hard);
    unlink(path);
    printf("chmodchown chmod=%d m600=%d fchmod=%d m755=%d chown=%d fchown=%d fstat=%d reopen=%d hard=%d statx=%d fork=%d\n",
           c1, is600, c2, is755, ch, fch, fstat_owner, reopen_owner, hard_owner, statx_owner, fork_owner);
    return 0;
}
