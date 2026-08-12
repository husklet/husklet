// A negative metadata lookup must not survive a successful O_CREAT. Database recovery code commonly probes a
// marker, creates it, then probes it again before cleanup; stale ENOENT turns a clean shutdown into crash recovery.
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    const char *path = "/tmp/hl-create-visibility";
    struct stat metadata;
    unlink(path);
    int before = stat(path, &metadata);
    int descriptor = open(path, O_CREAT | O_EXCL | O_RDWR, 0600);
    int after = stat(path, &metadata);
    long size = after == 0 ? (long)metadata.st_size : -1L;
    int removed = unlink(path);
    int final = stat(path, &metadata);
    if (descriptor >= 0) close(descriptor);
    printf("before=%d created=%d after=%d size=%ld removed=%d final=%d\n", before, descriptor >= 0, after, size,
           removed, final);
    return 0;
}
