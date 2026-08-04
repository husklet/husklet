#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char directory[] = "/tmp/hl-permissions-XXXXXX";
    if (!mkdtemp(directory) || chdir(directory) != 0) return 1;
    int fd = open("file", O_CREAT | O_RDWR, 0600);
    if (fd < 0 || write(fd, "x", 1) != 1 || fchmod(fd, 0000) != 0) return 2;
    close(fd);
    struct stat status;
    if (stat("file", &status) != 0 || (status.st_mode & 07777) != 0000) return 3;
    if (chmod("file", 02755) != 0 || stat("file", &status) != 0 || (status.st_mode & 07777) != 02755) return 4;
    if (symlink("file", "link") != 0 || lstat("link", &status) != 0 || (status.st_mode & 0777) != 0777) return 5;
    unlink("link");
    unlink("file");
    if (chdir("/") != 0) return 6;
    rmdir(directory);
    puts("permissions mode-zero=1 sgid=2755 symlink=777");
    return 0;
}
