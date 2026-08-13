#define _POSIX_C_SOURCE 200809L

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
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

/* Creating a file below a lower-only directory materializes that directory in
   the upper.  Copy-up must preserve all four mode octets, including /tmp's
   sticky bit, and must not let the process umask narrow its permissions. */
static int probe_directory_mode_copy_up(void) {
    struct stat status;
    pid_t child;
    int result;
    mode_t previous = umask(0077);
    int fd = open("/tmp/husklet-root", O_CREAT | O_EXCL | O_WRONLY, 0600);
    umask(previous);
    if (fd < 0 || close(fd) != 0) return 24;
    if (stat("/tmp", &status) != 0 || (status.st_mode & 07777) != 01777) return 25;
    child = fork();
    if (child < 0) return 26;
    if (child == 0) {
        if (setgid(65534) != 0 || setuid(65534) != 0) _exit(27);
        fd = open("/tmp/husklet-unprivileged", O_CREAT | O_EXCL | O_WRONLY, 0600);
        if (fd < 0 || close(fd) != 0) _exit(28);
        if (unlink("/tmp/husklet-root") == 0 || errno != EPERM) _exit(29);
        _exit(0);
    }
    if (waitpid(child, &result, 0) != child || !WIFEXITED(result) || WEXITSTATUS(result) != 0)
        return WIFEXITED(result) ? WEXITSTATUS(result) : 30;
    puts("lower directory mode copy-up ok");
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

/* Deleting a lower-only directory is the same whiteout problem one level up,
   and a name recreated over one must be opaque or the lower's children return. */
static int probe_whiteout_dir(void) {
    struct stat status;
    DIR *directory;
    struct dirent *entry;
    char victims[16][128];
    int count = 0;
    int saw_removed = 0;
    int saw_sibling = 0;

    /* The merged view decides the type and the emptiness, exactly as one
       filesystem would: no lower name may be masked on a mismatched request. */
    if (rmdir("/etc") == 0 || errno != ENOTEMPTY) return 50;
    if (unlink("/etc") == 0 || errno != EISDIR) return 51;
    if (rmdir("/etc/passwd") == 0 || errno != ENOTDIR) return 52;

    /* An empty lower-only directory goes away without the lower being touched. */
    if (rmdir("/srv") != 0) return 53;
    if (stat("/srv", &status) == 0 || errno != ENOENT) return 54;
    directory = opendir("/");
    if (!directory) return 55;
    while ((entry = readdir(directory))) {
        if (strcmp(entry->d_name, "srv") == 0) saw_removed = 1;
        if (strcmp(entry->d_name, "home") == 0) saw_sibling = 1;
    }
    if (closedir(directory) != 0) return 56;
    if (saw_removed) return 57;
    if (!saw_sibling) return 58;

    /* Recreating the name retires its marker. */
    if (mkdir("/srv", 0755) != 0) return 59;
    if (stat("/srv", &status) != 0 || !S_ISDIR(status.st_mode)) return 60;

    /* A lower-only parent must accept new names at all: nothing can land there
       until the upper copy of the parent has been materialized. */
    if (mkdir("/mnt/made", 0755) != 0) return 61;
    if (symlink("target", "/mnt/link") != 0) return 62;

    /* Remove and recreate a populated lower directory. The new one must be
       opaque, so not one of the lower's children may reappear inside it. */
    directory = opendir("/media");
    if (!directory) return 63;
    while ((entry = readdir(directory)) && count < 16) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) continue;
        snprintf(victims[count], sizeof victims[count], "/media/%s", entry->d_name);
        count++;
    }
    if (closedir(directory) != 0) return 64;
    if (count == 0) return 65;
    for (int i = 0; i < count; i++)
        if (rmdir(victims[i]) != 0) return 66;
    if (rmdir("/media") != 0) return 67;
    if (mkdir("/media", 0755) != 0) return 68;
    count = 0;
    directory = opendir("/media");
    if (!directory) return 69;
    while ((entry = readdir(directory)))
        if (strcmp(entry->d_name, ".") != 0 && strcmp(entry->d_name, "..") != 0) count++;
    if (closedir(directory) != 0) return 70;
    if (count != 0) return 71;

    puts("lower whiteout dir ok");
    return 0;
}

static int lock_range(int fd, int type, off_t start, off_t length) {
    struct flock request;
    memset(&request, 0, sizeof request);
    request.l_type = (short)type;
    request.l_whence = SEEK_SET;
    request.l_start = start;
    request.l_len = length;
    return fcntl(fd, F_SETLK, &request);
}

/* Forks a writer that opens the file read-write and must be refused the range. */
static int contend(off_t start, off_t length, int codes[3]) {
    int status;
    pid_t child = fork();
    if (child < 0) return -1;
    if (child == 0) {
        int code = 0;
        int upper = open("/etc/passwd", O_RDWR);
        if (upper < 0) code = codes[0];
        else if (lock_range(upper, F_WRLCK, start, length) == 0) code = codes[1];
        else if (errno != EAGAIN && errno != EACCES) code = codes[2];
        _exit(code);
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return -1;
    return WEXITSTATUS(status);
}

/* A lock on a lower file and a lock taken after copy-up are locks on one file.
   Both read locks are taken through a read-only descriptor, which is the only
   way to hold a lower inode: a write lock needs a writable fd, and opening one
   copies the file up by itself. The two ranges are disjoint so neither phase can
   be answered by the other's record. */
static int probe_lock_copy_up(void) {
    int first[3] = {82, 83, 84};
    int second[3] = {87, 88, 89};
    int result;
    int fd = open("/etc/passwd", O_RDONLY);
    if (fd < 0) return 80;
    /* Taken before any copy-up, so the record has to move to the upper inode. */
    if (lock_range(fd, F_RDLCK, 0, 100) != 0) return 81;
    result = contend(0, 100, first);
    if (result != 0) return result < 0 ? 85 : result;
    /* This descriptor still stats the lower, so a lock taken through it after the
       copy-up has to be translated instead of opening a second lock identity. */
    if (lock_range(fd, F_RDLCK, 200, 100) != 0) return 86;
    result = contend(200, 100, second);
    if (result != 0) return result < 0 ? 90 : result;
    if (close(fd) != 0) return 91;
    puts("lower lock copy-up ok");
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
    if (strcmp(arguments[1], "directory-mode-copy-up") == 0) return probe_directory_mode_copy_up();
    if (strcmp(arguments[1], "whiteout") == 0) return probe_whiteout();
    if (strcmp(arguments[1], "whiteout-dir") == 0) return probe_whiteout_dir();
    if (strcmp(arguments[1], "merge") == 0) return probe_merge();
    if (strcmp(arguments[1], "lock-copy-up") == 0) return probe_lock_copy_up();
    return 91;
}
