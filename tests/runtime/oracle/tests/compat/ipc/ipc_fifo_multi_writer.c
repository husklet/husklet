// One FIFO, two concurrent writers: two forked children each stream 1000 ints, the parent sums all
// 2000 values from the single read end. Verifies interleaved writers on a FIFO. Portable, golden.
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
int main(void) {
    const char *path = "/tmp/hl_fifo_mw";
    int ready[2], start[2];
    unlink(path);
    if (mkfifo(path, 0644) != 0 || pipe(ready) != 0 || pipe(start) != 0) return 1;
    // Keep one reader present while both children enter open(O_WRONLY), then release them together.
    // Without this barrier the first child may finish before the second reaches open, making the test
    // scheduler-dependent instead of testing concurrent FIFO writers.
    int anchor = open(path, O_RDWR);
    for (int k = 0; k < 2; k++) {
        if (fork() == 0) {
            char token;
            close(anchor);
            close(ready[0]); close(start[1]);
            int w = open(path, O_WRONLY);
            if (w < 0 || write(ready[1], "r", 1) != 1 || read(start[0], &token, 1) != 1) _exit(3);
            for (int i = 1; i <= 1000; i++)
                if (write(w, &i, sizeof i) != sizeof i) _exit(4);
            close(w); _exit(0);
        }
    }
    close(ready[1]); close(start[0]);
    char tokens[2];
    int ready_count = 0;
    while (ready_count < 2) {
        int count = (int)read(ready[0], tokens + ready_count, (size_t)(2 - ready_count));
        if (count <= 0) return 2;
        ready_count += count;
    }
    int r = open(path, O_RDONLY);
    close(anchor);
    if (write(start[1], "ss", 2) != 2) return 3;
    close(ready[0]); close(start[1]);
    long sum = 0; int v; int got = 0;
    while (got < 2000 && read(r, &v, sizeof v) == sizeof v) { sum += v; got++; }
    close(r);
    for (int k = 0; k < 2; k++) wait(0);
    unlink(path);
    printf("fifo_mw sum=%ld got=%d\n", sum, got); // 2*(1..1000) = 1001000, 2000
    return 0;
}
