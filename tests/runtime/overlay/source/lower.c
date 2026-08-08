#define _POSIX_C_SOURCE 200809L

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Every corpus binary is statically linked and staged into the overlay upper, so
   none of them resolves a name in the immutable lower. These probes do, which is
   the only way the union walk and the lower name index are exercised at all. */

static int probe_read(void) {
    struct stat status;
    char bytes[512];
    int fd;
    ssize_t got;
    if (stat("/etc/passwd", &status) != 0 || status.st_size <= 0) return 1;
    fd = open("/etc/passwd", O_RDONLY);
    if (fd < 0) return 2;
    got = read(fd, bytes, sizeof bytes - 1);
    if (got <= 0 || close(fd) != 0) return 3;
    bytes[got] = '\0';
    if (strncmp(bytes, "root:", 5) != 0) return 4;
    if (stat("/bin/busybox", &status) != 0 || status.st_size < 100000) return 5;
    puts("lower read ok");
    return 0;
}

/* The negative-probe storm an interpreter's library search produces: every name
   is absent from the lower, which is exactly the lookup the index answers. */
static int probe_negative(void) {
    char path[128];
    struct stat status;
    for (int i = 0; i < 2000; i++) {
        snprintf(path, sizeof path, "/usr/lib/libabsent%04d.so.1", i);
        if (stat(path, &status) == 0) return 10;
        if (errno != ENOENT) return 11;
    }
    /* A name that is present must still resolve, so absence is not answered blindly. */
    if (stat("/usr/lib", &status) != 0) return 12;
    puts("lower negative ok");
    return 0;
}

/* Writing through a lower file must copy it up: the write is visible on reopen
   and the lower's own bytes are unchanged for everyone else. */
static int probe_copy_up(void) {
    char bytes[8];
    int fd = open("/etc/passwd", O_RDWR);
    if (fd < 0) return 20;
    if (pwrite(fd, "ZZZZ", 4, 0) != 4 || close(fd) != 0) return 21;
    fd = open("/etc/passwd", O_RDONLY);
    if (fd < 0 || read(fd, bytes, 4) != 4 || close(fd) != 0) return 22;
    if (memcmp(bytes, "ZZZZ", 4) != 0) return 23;
    puts("lower copy-up ok");
    return 0;
}

/* Removing a lower name must whiteout: gone from lookup and from the merged
   directory listing, while its lower siblings remain visible. */
static int probe_whiteout(void) {
    struct stat status;
    DIR *directory;
    struct dirent *entry;
    int saw_removed = 0;
    int saw_sibling = 0;
    if (unlink("/etc/passwd") != 0) return 30;
    if (stat("/etc/passwd", &status) == 0 || errno != ENOENT) return 31;
    directory = opendir("/etc");
    if (!directory) return 32;
    while ((entry = readdir(directory))) {
        if (strcmp(entry->d_name, "passwd") == 0) saw_removed = 1;
        if (strcmp(entry->d_name, "group") == 0) saw_sibling = 1;
    }
    if (closedir(directory) != 0) return 33;
    if (saw_removed) return 34;
    if (!saw_sibling) return 35;
    puts("lower whiteout ok");
    return 0;
}

/* A directory that exists in both layers must list the union of their children. */
static int probe_merge(void) {
    DIR *directory;
    struct dirent *entry;
    int saw_upper = 0;
    int saw_lower = 0;
    int fd = open("/etc/husklet-upper-only", O_CREAT | O_EXCL | O_WRONLY, 0644);
    if (fd < 0 || close(fd) != 0) return 40;
    directory = opendir("/etc");
    if (!directory) return 41;
    while ((entry = readdir(directory))) {
        if (strcmp(entry->d_name, "husklet-upper-only") == 0) saw_upper = 1;
        if (strcmp(entry->d_name, "group") == 0) saw_lower = 1;
    }
    if (closedir(directory) != 0) return 42;
    if (!saw_upper || !saw_lower) return 43;
    puts("lower merge ok");
    return 0;
}

int main(int count, char **arguments) {
    if (count != 2) return 90;
    if (strcmp(arguments[1], "read") == 0) return probe_read();
    if (strcmp(arguments[1], "negative") == 0) return probe_negative();
    if (strcmp(arguments[1], "copy-up") == 0) return probe_copy_up();
    if (strcmp(arguments[1], "whiteout") == 0) return probe_whiteout();
    if (strcmp(arguments[1], "merge") == 0) return probe_merge();
    return 91;
}
