// syscall-compat coverage: umask affects create mode and returns the prior mask. Setting umask 077 then
// creating a 0666 file yields mode 0600; restoring the mask returns 077. mkdir honors the umask too.
// Arch-neutral: octal permission bits / booleans printed.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[] = "/tmp/umask_XXXXXX";
    mkdtemp(dir);
    mode_t prev = umask(077);

    char file[256], sub[256];
    snprintf(file, sizeof(file), "%s/f", dir);
    snprintf(sub, sizeof(sub), "%s/d", dir);
    int fd = open(file, O_CREAT | O_WRONLY, 0666);
    close(fd);
    mkdir(sub, 0777);

    struct stat fs, ds;
    stat(file, &fs);
    stat(sub, &ds);
    printf("file_mode=%o\n", fs.st_mode & 0777);
    printf("dir_mode=%o\n", ds.st_mode & 0777);

    mode_t got = umask(prev);
    printf("prev_was_077=%d\n", got == 077);

    unlink(file); rmdir(sub); rmdir(dir);
    return 0;
}
