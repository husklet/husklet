#define _POSIX_C_SOURCE 200809L

#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    /* GNU cp -p's metadata sequence on an overlay destination.  Package
       post-install scripts depend on all three operations succeeding after a
       lower-layer source is copied into a newly-created upper-layer file. */
    int source = open("/etc/hostname", O_RDONLY);
    int copy = open("/etc/hostname.copy", O_CREAT | O_EXCL | O_WRONLY, 0600);
    char copy_bytes[128];
    ssize_t copied;
    if (source < 0 || copy < 0) return 1;
    while ((copied = read(source, copy_bytes, sizeof copy_bytes)) > 0)
        if (write(copy, copy_bytes, (size_t)copied) != copied) return 2;
    if (copied < 0 || close(source) != 0) return 3;
    if (fchown(copy, 0, 0) != 0) return 4;
    if (fchmod(copy, 0644) != 0) return 5;
    struct timespec preserved[2] = {{1700000000, 123000000}, {1700000001, 456000000}};
    if (futimens(copy, preserved) != 0 || close(copy) != 0) return 6;
    struct stat copy_status;
    if (stat("/etc/hostname.copy", &copy_status) != 0 ||
        (copy_status.st_mode & 07777) != 0644 || copy_status.st_uid != 0 || copy_status.st_gid != 0 ||
        copy_status.st_mtim.tv_sec != preserved[1].tv_sec) return 7;

    int fd = open("/etc/hostname", O_RDWR);
    if (fd < 0) return 10;
    struct stat before;
    if (fstat(fd, &before) != 0 || before.st_size < 4) return 11;
    char *bytes = mmap(NULL, (size_t)before.st_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (bytes == MAP_FAILED) return 12;
    memcpy(bytes, "MM", 2);
    if (msync(bytes, 2, MS_SYNC) != 0 || munmap(bytes, (size_t)before.st_size) != 0) return 13;
    if (ftruncate(fd, 4) != 0 || close(fd) != 0) return 14;
    if (rename("/etc/hostname", "/etc/hostname.moved") != 0) return 15;
    fd = open("/etc/hostname.moved", O_RDONLY);
    char result[5] = {0};
    if (fd < 0 || read(fd, result, 4) != 4 || close(fd) != 0) return 16;
    if (memcmp(result, "MMca", 4) != 0) return 17;
    puts("overlay coherence ok");
    return 0;
}
