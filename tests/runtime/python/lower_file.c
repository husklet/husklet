#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static int merged_listing_ok(int directory) {
    struct linux_dirent64 {
        unsigned long long inode;
        long long offset;
        unsigned short record_length;
        unsigned char type;
        char name[];
    };

    char entries[4096];
    int aliases = 0, cache = 0;
    long count;
    while ((count = syscall(SYS_getdents64, directory, entries, sizeof entries)) > 0) {
        for (long offset = 0; offset < count;) {
            struct linux_dirent64 *entry = (struct linux_dirent64 *)(entries + offset);
            if (entry->record_length == 0 || offset + entry->record_length > count) return -1;
            aliases |= strcmp(entry->name, "aliases.py") == 0;
            cache |= strcmp(entry->name, "__pycache__") == 0;
            offset += entry->record_length;
        }
    }
    if (count < 0 || !aliases || !cache) {
        fprintf(stderr, "merged listing: errno=%d aliases=%d cache=%d\n", errno, aliases, cache);
        return -1;
    }
    return 0;
}

int main(void) {
    static const char path[] = "/usr/local/lib/python3.12/encodings/aliases.py";
    struct stat status;
    char first[64], second[64];
    int left = open(path, O_RDONLY | O_CLOEXEC);
    int right = open(path, O_RDONLY);
    if (left < 0 || right < 0) {
        fprintf(stderr, "open: %s\n", strerror(errno));
        return 1;
    }
    if ((fcntl(left, F_GETFD) & FD_CLOEXEC) == 0) return 10;
    if (fstat(left, &status) != 0 || status.st_size < 1000) return 2;
    ssize_t left_count = read(left, first, sizeof first);
    ssize_t right_count = read(right, second, sizeof second);
    if (left_count != (ssize_t)sizeof first || right_count != (ssize_t)sizeof second) return 3;
    if (memcmp(first, second, sizeof first) != 0) return 4;
    if (memcmp(first, "\"\"\" Encoding Aliases Support", 28) != 0) return 11;
    if (lseek(left, 0, SEEK_SET) != 0 || read(left, first, sizeof first) != (ssize_t)sizeof first) return 5;
    if (lseek(left, 0, SEEK_SET) != 0) return 12;
    char whole[20000];
    size_t total = 0;
    ssize_t count;
    while ((count = read(left, whole + total, sizeof whole - total)) > 0)
        total += (size_t)count;
    if (count != 0 || total != (size_t)status.st_size || total != 15677) return 13;
    void *mapping = mmap(NULL, (size_t)status.st_size, PROT_READ, MAP_PRIVATE, right, 0);
    if (mapping == MAP_FAILED || memcmp(mapping, whole, total) != 0) return 14;
    if (munmap(mapping, (size_t)status.st_size) != 0) return 15;
    if (close(left) != 0 || close(right) != 0) return 6;
    int directory = open("/usr/local/lib/python3.12/encodings", O_RDONLY | O_DIRECTORY);
    if (directory < 0) return 7;
    left = openat(directory, "aliases.py", O_RDONLY);
    if (left < 0 || read(left, first, sizeof first) != (ssize_t)sizeof first) return 8;
    if (close(left) != 0 || close(directory) != 0) return 9;
    int upper = open("/usr/local/lib/python3.12/encodings/husklet-upper", O_CREAT | O_WRONLY, 0600);
    if (upper < 0 || close(upper) != 0) return 16;
    left = open(path, O_RDONLY);
    if (left < 0) {
        fprintf(stderr, "reopen after upper sibling: %s\n", strerror(errno));
        return 17;
    }
    if (read(left, first, sizeof first) != (ssize_t)sizeof first ||
        memcmp(first, "\"\"\" Encoding Aliases Support", 28) != 0)
        return 18;
    if (close(left) != 0) return 19;
    upper = open("/usr/local/lib/python3.12/encodings/__pycache__/husklet.pyc", O_CREAT | O_WRONLY, 0600);
    if (upper < 0 || write(upper, "cache", 5) != 5 || close(upper) != 0) return 20;
    left = open(path, O_RDONLY);
    if (left < 0 || read(left, first, sizeof first) != (ssize_t)sizeof first ||
        memcmp(first, "\"\"\" Encoding Aliases Support", 28) != 0)
        return 21;
    if (close(left) != 0) return 22;
    upper = open("/usr/local/lib/python3.12/encodings/__pycache__/husklet.tmp", O_CREAT | O_EXCL | O_WRONLY, 0600);
    if (upper < 0 || write(upper, "cache2", 6) != 6 || fsync(upper) != 0 || close(upper) != 0) return 23;
    if (rename("/usr/local/lib/python3.12/encodings/__pycache__/husklet.tmp",
               "/usr/local/lib/python3.12/encodings/__pycache__/husklet-renamed.pyc") != 0)
        return 24;
    left = open(path, O_RDONLY);
    if (left < 0 || read(left, first, sizeof first) != (ssize_t)sizeof first ||
        memcmp(first, "\"\"\" Encoding Aliases Support", 28) != 0)
        return 25;
    if (close(left) != 0) return 26;
    left = open("/usr/local/lib/python3.12/encodings/__pycache__/husklet-renamed.pyc", O_RDONLY);
    if (left < 0 || read(left, first, 6) != 6 || memcmp(first, "cache2", 6) != 0 || close(left) != 0) return 27;
    directory = open("/usr/local/lib/python3.12/encodings", O_RDONLY | O_DIRECTORY);
    int cache = openat(directory, "__pycache__", O_RDONLY | O_DIRECTORY);
    if (directory < 0 || cache < 0) return 28;
    upper = openat(cache, "husklet-at.tmp", O_CREAT | O_EXCL | O_WRONLY, 0600);
    if (upper < 0 || write(upper, "cache3", 6) != 6 || fsync(upper) != 0 || close(upper) != 0) return 29;
    left = openat(directory, "aliases.py", O_RDONLY | O_CLOEXEC);
    if (left < 0 || close(left) != 0) {
        fprintf(stderr, "openat after create: fd=%d errno=%d\n", left, errno);
        return 33;
    }
    if (renameat(cache, "husklet-at.tmp", cache, "husklet-at.pyc") != 0) return 30;
    left = openat(directory, "aliases.py", O_RDONLY | O_CLOEXEC);
    ssize_t after_rename = left < 0 ? -1 : read(left, first, sizeof first);
    if (left < 0 || after_rename != (ssize_t)sizeof first ||
        memcmp(first, "\"\"\" Encoding Aliases Support", 28) != 0) {
        fprintf(stderr, "openat after rename: fd=%d errno=%d read=%zd prefix=%.28s\n", left, errno, after_rename,
                left < 0 || after_rename <= 0 ? "" : first);
        return 31;
    }
    if (close(left) != 0 || close(cache) != 0 || close(directory) != 0) return 32;
    directory = open("/usr/local/lib/python3.12/encodings", O_RDONLY | O_DIRECTORY);
    if (directory < 0) return 34;
    if (merged_listing_ok(directory) != 0) return 51;
    left = openat(directory, "aliases.py", O_RDONLY | O_CLOEXEC);
    if (left < 0 || read(left, first, sizeof first) != (ssize_t)sizeof first ||
        memcmp(first, "\"\"\" Encoding Aliases Support", 28) != 0)
        return 35;
    if (close(left) != 0 || close(directory) != 0) return 36;
    puts("python lower file ok");
    return 0;
}
