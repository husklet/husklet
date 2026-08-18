#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

struct payload {
    uint32_t cycle;
    uint32_t sentinel;
    uint64_t checksum;
};

static void fail(const char *operation) {
    dprintf(STDERR_FILENO, "FAIL %s errno=%d\n", operation, errno);
    _exit(70);
}

static void path(char *output, size_t capacity, const char *root, const char *name) {
    if (snprintf(output, capacity, "%s.%s", root, name) >= (int)capacity) fail("path");
}

static void publish(const char *root, const char *name) {
    char marker[1024];
    path(marker, sizeof marker, root, name);
    int descriptor = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (descriptor < 0 && errno != EEXIST) fail("publish");
    if (descriptor >= 0) close(descriptor);
}

static void await(const char *root, const char *name) {
    char marker[1024];
    path(marker, sizeof marker, root, name);
    while (access(marker, F_OK) != 0) {
        if (errno != ENOENT) fail("await");
        usleep(1000);
    }
}

static void write_exact(int descriptor, const void *bytes, size_t size) {
    const unsigned char *cursor = bytes;
    while (size != 0) {
        ssize_t count = write(descriptor, cursor, size);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) fail("write-exact");
        cursor += count;
        size -= (size_t)count;
    }
}

static void read_exact(int descriptor, void *bytes, size_t size) {
    unsigned char *cursor = bytes;
    while (size != 0) {
        ssize_t count = read(descriptor, cursor, size);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) fail("read-exact");
        cursor += count;
        size -= (size_t)count;
    }
}

static struct payload payload(unsigned cycle) {
    struct payload value = {.cycle = cycle, .sentinel = UINT32_C(0x48504c50)};
    value.checksum = UINT64_C(0x9e3779b97f4a7c15) ^ value.cycle ^ value.sentinel;
    return value;
}

static int identity_only(void) {
    return getenv("HL_OFD_IDENTITY_ONLY") != NULL;
}

static void create_identity_pipe(void) {
    int descriptors[2];
    if (pipe(descriptors) != 0 || dup2(descriptors[0], 30) < 0 || dup2(descriptors[1], 31) < 0) fail("identity-pipe");
    close(descriptors[0]);
    close(descriptors[1]);
}

static void consume(unsigned cycle) {
    struct payload observed;
    read_exact(4, &observed, sizeof observed);
    struct payload expected = payload(cycle);
    if (memcmp(&observed, &expected, sizeof observed) != 0) fail("payload");
    int duplicate = dup(4);
    int flags = fcntl(duplicate, F_GETFL);
    if (duplicate < 0 || flags < 0 || fcntl(duplicate, F_SETFL, flags | O_NONBLOCK) != 0) fail("empty-flags");
    unsigned char byte;
    errno = 0;
    if (read(duplicate, &byte, 1) != -1 || errno != EAGAIN) fail("queue-not-empty");
    if (fcntl(duplicate, F_SETFL, flags) != 0) fail("empty-flags-restore");
    close(duplicate);
}

static void ofd_writer(const char *root, const char *prefix) {
    int flags = fcntl(4, F_GETFL);
    if (flags < 0 || fcntl(4, F_SETFL, flags | O_NONBLOCK) != 0 || fcntl(4, F_SETFD, FD_CLOEXEC) != 0)
        fail("ofd-writer");
    char marker[64];
    snprintf(marker, sizeof marker, "%s-set", prefix);
    publish(root, marker);
    snprintf(marker, sizeof marker, "%s-seen", prefix);
    await(root, marker);
    if (fcntl(4, F_SETFL, flags) != 0) fail("ofd-restore");
    snprintf(marker, sizeof marker, "%s-clear", prefix);
    publish(root, marker);
}

static void ofd_observer(const char *root, const char *prefix) {
    char marker[64];
    snprintf(marker, sizeof marker, "%s-set", prefix);
    await(root, marker);
    if ((fcntl(4, F_GETFL) & O_NONBLOCK) == 0) fail("ofd-not-shared");
    if ((fcntl(4, F_GETFD) & FD_CLOEXEC) != 0) fail("descriptor-flags-shared");
    snprintf(marker, sizeof marker, "%s-seen", prefix);
    publish(root, marker);
    snprintf(marker, sizeof marker, "%s-clear", prefix);
    await(root, marker);
    if ((fcntl(4, F_GETFL) & O_NONBLOCK) != 0) fail("ofd-not-restored");
}

static void descriptor_state(int role, int expected_writer_cloexec) {
    int descriptor_flags = fcntl(4, F_GETFD);
    if (descriptor_flags < 0 || ((descriptor_flags & FD_CLOEXEC) != 0) != expected_writer_cloexec)
        fail("descriptor-state");
    dprintf(STDOUT_FILENO, "PIPE-FLAGS role=%d cloexec=%d\n", role, expected_writer_cloexec);
}

static int child(const char *release, const char *final_release, int role) {
    close(5);
    if (role != 2) close(7);
    if (fcntl(5, F_GETFD) != -1 || errno != EBADF) return 80 + role;
    if (!identity_only() && role == 0) ofd_writer(release, "before");
    if (!identity_only() && role == 1) ofd_observer(release, "before");
    dprintf(STDOUT_FILENO, "PIPE-READY %d pid=%ld\n", role, (long)getpid());
    await(release, "go");
    descriptor_state(role, !identity_only() && role == 0);
    if (!identity_only() && role == 0) ofd_writer(release, "after-first");
    if (!identity_only() && role == 1) ofd_observer(release, "after-first");
    if (role == 0) {
        consume(1);
        publish(release, "consumed-first");
        dprintf(STDOUT_FILENO, "PIPE-CONSUMED 1 pid=%ld\n", (long)getpid());
    }
    create_identity_pipe();
    await(release, "cycle-two-ready");
    dprintf(STDOUT_FILENO, "PIPE-CYCLE-READY %d pid=%ld\n", role, (long)getpid());
    await(final_release, "go");
    descriptor_state(role, !identity_only() && role == 0);
    if (!identity_only() && role == 1) ofd_writer(final_release, "after-second");
    if (!identity_only() && role == 2) ofd_observer(final_release, "after-second");
    if (role == 1) {
        consume(2);
        publish(final_release, "consumed-second");
        dprintf(STDOUT_FILENO, "PIPE-CONSUMED 2 pid=%ld\n", (long)getpid());
    }
    if (role == 2) {
        await(final_release, "writer-closed");
        unsigned char byte;
        ssize_t count;
        do
            count = read(4, &byte, 1);
        while (count < 0 && errno == EINTR);
        if (count != 0) fail("pipe-eof");
        dprintf(STDOUT_FILENO, "PIPE-EOF pid=%ld\n", (long)getpid());
    }
    close(7);
    close(30);
    close(31);
    close(4);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    int subject[2];
    if (pipe(subject) != 0) fail("pipe");
    int reader = fcntl(subject[0], F_DUPFD_CLOEXEC, 20);
    int writer = fcntl(subject[1], F_DUPFD_CLOEXEC, 20);
    close(subject[0]);
    close(subject[1]);
    if (reader < 0 || writer < 0 || dup2(reader, 4) < 0 || dup2(reader, 7) < 0 || dup2(writer, 5) < 0)
        fail("normalize-pipe");
    close(reader);
    close(writer);

    struct payload first = payload(1);
    write_exact(5, &first, sizeof first);
    pid_t children[3];
    for (int role = 0; role < 3; ++role) {
        children[role] = fork();
        if (children[role] < 0) fail("fork");
        if (children[role] == 0) _exit(child(argv[1], argv[2], role));
    }
    dprintf(STDOUT_FILENO, "PIPE-READY parent pid=%ld\n", (long)getpid());
    await(argv[1], "consumed-first");
    create_identity_pipe();
    struct payload second = payload(2);
    write_exact(5, &second, sizeof second);
    publish(argv[1], "cycle-two-ready");
    await(argv[2], "consumed-second");
    close(5);
    close(4);
    close(7);
    close(30);
    close(31);
    publish(argv[2], "writer-closed");
    int result = 0;
    for (int role = 0; role < 3; ++role) {
        int status;
        if (waitpid(children[role], &status, 0) != children[role] || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
            result = 90 + role;
    }
    if (result == 0) dprintf(STDOUT_FILENO, "PIPE-TREE-RESTORED\n");
    return result;
}
