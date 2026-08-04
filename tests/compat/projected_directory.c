#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

struct linux_dirent64 {
    unsigned long long inode;
    long long offset;
    unsigned short length;
    unsigned char type;
    char name[];
};

static int contains(const char *buffer, int count, const char *wanted) {
    for (int offset = 0; offset < count;) {
        struct linux_dirent64 *entry = (void *)(buffer + offset);
        if (entry->length < 20 || offset + entry->length > count) return 0;
        if (!strcmp(entry->name, wanted)) return 1;
        offset += entry->length;
    }
    return 0;
}

int main(void) {
    char bytes[512], link[32];
    int directory = open("tree/base", O_RDONLY | O_DIRECTORY);
    int child = openat(directory, "child", O_RDONLY | O_DIRECTORY);
    int file = openat(child, "../value", O_RDONLY);
    int relative = openat(directory, "relative/value", O_RDONLY);
    int absolute = openat(directory, "absolute/value", O_RDONLY);
    errno = 0;
    int nofollow = openat(directory, "relative", O_RDONLY | O_NOFOLLOW);
    int loop_ok = nofollow == -1 && errno == ELOOP;
    errno = 0;
    ssize_t link_count = readlinkat(directory, "relative", link, sizeof link);
    int link_errno = errno;
    errno = 0;
    int tiny = syscall(SYS_getdents64, directory, bytes, 1);
    int tiny_ok = tiny == -1 && errno == EINVAL;
    int count = syscall(SYS_getdents64, directory, bytes, sizeof bytes);
    int entries = count > 0 && contains(bytes, count, ".") && contains(bytes, count, "..")
        && contains(bytes, count, "child") && contains(bytes, count, "value");
    char value[2];
    int reads = read(file, value, 1) == 1 && value[0] == 'v'
        && read(relative, value, 1) == 1 && value[0] == 'v'
        && read(absolute, value, 1) == 1 && value[0] == 'v';
    int links = link_count == 5 && !memcmp(link, "child", 5);
    printf("projected-directory reads=%d links=%d loop=%d tiny=%d entries=%d count=%ld errno=%d\n",
        reads, links, loop_ok, tiny_ok, entries, (long)link_count, link_errno);
    return !(reads && links && loop_ok && tiny_ok && entries);
}
