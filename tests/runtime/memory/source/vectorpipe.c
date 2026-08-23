#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

enum { RECORDS = 32, RECORD_SIZE = 8 };

static int write_records(int fd, char value) {
    char left[4], right[4];
    memset(left, value, sizeof left);
    memset(right, value, sizeof right);
    struct iovec vectors[] = {{left, sizeof left}, {right, sizeof right}};
    for (int index = 0; index < RECORDS; ++index) {
        if (writev(fd, vectors, 2) != RECORD_SIZE) return 1;
    }
    return 0;
}

int main(void) {
    int pipes[2];
    if (pipe(pipes) != 0) return 2;
    pid_t first = fork();
    if (first < 0) return 3;
    if (first == 0) {
        close(pipes[0]);
        _exit(write_records(pipes[1], 'A'));
    }
    pid_t second = fork();
    if (second < 0) return 4;
    if (second == 0) {
        close(pipes[0]);
        _exit(write_records(pipes[1], 'B'));
    }
    close(pipes[1]);
    unsigned char records[RECORDS * 2][RECORD_SIZE];
    size_t received = 0;
    while (received < sizeof records) {
        ssize_t count = read(pipes[0], (unsigned char *)records + received, sizeof records - received);
        if (count <= 0) return 5;
        received += (size_t)count;
    }
    int status_first = 0, status_second = 0;
    waitpid(first, &status_first, 0);
    waitpid(second, &status_second, 0);
    int atomic = WIFEXITED(status_first) && WEXITSTATUS(status_first) == 0 && WIFEXITED(status_second) &&
                 WEXITSTATUS(status_second) == 0;
    for (size_t record = 0; record < RECORDS * 2 && atomic; ++record) {
        unsigned char value = records[record][0];
        atomic = (value == 'A' || value == 'B');
        for (size_t byte = 1; byte < RECORD_SIZE; ++byte)
            atomic = atomic && records[record][byte] == value;
    }

    int split[2];
    if (pipe(split) != 0 || write(split[1], "abcdef", 6) != 6) return 6;
    char left[2] = {0}, right[4] = {0};
    struct iovec output[] = {{left, sizeof left}, {right, sizeof right}};
    int distributed = readv(split[0], output, 2) == 6 && memcmp(left, "ab", 2) == 0 && memcmp(right, "cdef", 4) == 0;
    printf("vector-pipe atomic=%d distributed=%d\n", atomic, distributed);
    return atomic && distributed ? 0 : 1;
}
