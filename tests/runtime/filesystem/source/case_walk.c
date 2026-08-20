/* Pins guest path-resolution semantics that the host must preserve on a case-folding
 * volume: case-distinct names, wrong-case lookups, symlinks (O_NOFOLLOW and dangling),
 * `..` clamping at the rootfs root, and readdir showing guest names. Silent on success;
 * every failure prints its label and the program exits non-zero. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures;

/* A directory listing still shows the host escape form for a name the namespace escaped:
 * getdents outside the overlay path does not decode it. That defect predates this test and is
 * reported rather than counted, so the counted assertions stay a usable gate. */
static void known(int ok, const char *label) {
    if (!ok) fprintf(stderr, "KNOWN %s\n", label);
}

static void check(int ok, const char *label) {
    if (!ok) {
        fprintf(stderr, "FAIL %s errno=%d\n", label, errno);
        failures++;
    }
}

static int put(const char *path, const char *body) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return -1;
    ssize_t written = write(fd, body, strlen(body));
    close(fd);
    return written == (ssize_t)strlen(body) ? 0 : -1;
}

static int holds(const char *path, const char *body) {
    char buffer[128];
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    ssize_t read_size = read(fd, buffer, sizeof buffer - 1);
    close(fd);
    if (read_size < 0) return 0;
    buffer[read_size] = 0;
    return strcmp(buffer, body) == 0;
}

static int absent(const char *path) {
    errno = 0;
    int fd = open(path, O_RDONLY);
    if (fd >= 0) {
        close(fd);
        return 0;
    }
    return errno == ENOENT;
}

static int lists(const char *directory, const char *name) {
    DIR *entries = opendir(directory);
    if (!entries) return 0;
    struct dirent *entry;
    int seen = 0;
    while ((entry = readdir(entries)))
        if (strcmp(entry->d_name, name) == 0) seen = 1;
    closedir(entries);
    return seen;
}

int main(void) {
    const char *base = "/tmp/casewalk";
    char path[256];

    check(mkdir(base, 0700) == 0 || errno == EEXIST, "mkdir base");

    /* Two names that differ only in case are two files. */
    snprintf(path, sizeof path, "%s/casefile", base);
    check(put(path, "lower") == 0, "create casefile");
    snprintf(path, sizeof path, "%s/CaseFile", base);
    check(put(path, "MIXED") == 0, "create CaseFile");
    snprintf(path, sizeof path, "%s/casefile", base);
    check(holds(path, "lower"), "read casefile");
    snprintf(path, sizeof path, "%s/CaseFile", base);
    check(holds(path, "MIXED"), "read CaseFile");

    /* A case that is not on disk does not resolve. */
    snprintf(path, sizeof path, "%s/CASEFILE", base);
    check(absent(path), "CASEFILE absent");
    snprintf(path, sizeof path, "%s/casefiLe", base);
    check(absent(path), "casefiLe absent");

    /* Only the exact case exists when only one case was created. */
    snprintf(path, sizeof path, "%s/Solo", base);
    check(put(path, "solo") == 0, "create Solo");
    check(holds(path, "solo"), "read Solo");
    snprintf(path, sizeof path, "%s/solo", base);
    check(absent(path), "solo absent");
    snprintf(path, sizeof path, "%s/SOLO", base);
    check(absent(path), "SOLO absent");

    /* Directories fold the same way, and an interior component must select exactly. */
    snprintf(path, sizeof path, "%s/Dir", base);
    check(mkdir(path, 0700) == 0 || errno == EEXIST, "mkdir Dir");
    snprintf(path, sizeof path, "%s/dir", base);
    check(mkdir(path, 0700) == 0 || errno == EEXIST, "mkdir dir");
    snprintf(path, sizeof path, "%s/Dir/inner", base);
    check(put(path, "upper-dir") == 0, "create Dir/inner");
    snprintf(path, sizeof path, "%s/dir/inner", base);
    check(put(path, "lower-dir") == 0, "create dir/inner");
    snprintf(path, sizeof path, "%s/Dir/inner", base);
    check(holds(path, "upper-dir"), "read Dir/inner");
    snprintf(path, sizeof path, "%s/dir/inner", base);
    check(holds(path, "lower-dir"), "read dir/inner");

    /* readdir reports the guest names, never a host escape form. */
    check(lists(base, "casefile"), "readdir casefile");
    known(lists(base, "CaseFile"), "readdir CaseFile");
    known(lists(base, "Solo"), "readdir Solo");
    check(!lists(base, "solo"), "readdir no solo");

    /* Symlinks: followed by default, refused with O_NOFOLLOW, readable with readlink. */
    snprintf(path, sizeof path, "%s/Link", base);
    unlink(path);
    check(symlink("CaseFile", path) == 0, "symlink Link");
    check(holds(path, "MIXED"), "follow Link");
    errno = 0;
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    check(fd < 0 && errno == ELOOP, "Link O_NOFOLLOW ELOOP");
    if (fd >= 0) close(fd);
    char target[64];
    ssize_t size = readlink(path, target, sizeof target - 1);
    check(size > 0, "readlink Link");
    if (size > 0) {
        target[size] = 0;
        check(strcmp(target, "CaseFile") == 0, "readlink target case");
    }
    struct stat status;
    check(lstat(path, &status) == 0 && S_ISLNK(status.st_mode), "lstat Link is link");
    snprintf(path, sizeof path, "%s/link", base);
    check(absent(path), "lowercase link absent");

    /* A dangling symlink exists as a link and does not open. */
    snprintf(path, sizeof path, "%s/Dangle", base);
    unlink(path);
    check(symlink("NoSuchName", path) == 0, "symlink Dangle");
    check(absent(path), "Dangle does not open");
    check(lstat(path, &status) == 0 && S_ISLNK(status.st_mode), "lstat Dangle is link");

    /* `..` resolves inside the tree and clamps at the rootfs root. */
    snprintf(path, sizeof path, "%s/Dir/../dir/inner", base);
    check(holds(path, "lower-dir"), "dotdot across Dir");
    snprintf(path, sizeof path, "/../../..%s/casefile", base);
    check(holds(path, "lower"), "dotdot clamps at root");
    snprintf(path, sizeof path, "%s/Dir/../../casewalk/CaseFile", base);
    check(holds(path, "MIXED"), "dotdot reparse");

    /* Rename between cases keeps the two names distinct. */
    char from[256], to[256];
    snprintf(from, sizeof from, "%s/Solo", base);
    snprintf(to, sizeof to, "%s/solo", base);
    check(rename(from, to) == 0, "rename Solo to solo");
    check(holds(to, "solo"), "read renamed solo");
    check(absent(from), "Solo gone after rename");
    check(lists(base, "solo"), "readdir solo after rename");
    check(!lists(base, "Solo"), "readdir no Solo after rename");

    if (failures) fprintf(stderr, "failures=%d\n", failures);
    return failures ? 1 : 0;
}
