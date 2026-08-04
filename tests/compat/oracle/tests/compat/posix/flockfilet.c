// flockfile/funlockfile with getc_unlocked/putc_unlocked from concurrent threads keep the stream consistent.
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <stdlib.h>

#define N 4
#define PER 500
static FILE *fp;

static void *writer(void *arg) {
    char c = *(char *)arg;
    for (int i = 0; i < PER; i++) {
        flockfile(fp);
        // Write a 2-char record atomically under the explicit lock.
        putc_unlocked(c, fp);
        putc_unlocked(c, fp);
        funlockfile(fp);
    }
    return NULL;
}

int main(void) {
    char path[64];
    snprintf(path, sizeof path, "/tmp/hl_flf_%d", (int)getpid());
    fp = fopen(path, "w+");
    if (!fp) { printf("flockfilet open=0\n"); return 0; }

    char ids[N] = {'a', 'b', 'c', 'd'};
    pthread_t t[N];
    for (int i = 0; i < N; i++) pthread_create(&t[i], NULL, writer, &ids[i]);
    for (int i = 0; i < N; i++) pthread_join(t[i], NULL);
    fflush(fp);

    // Read back with getc_unlocked; every record must be a doubled char (no interleave).
    rewind(fp);
    flockfile(fp);
    int total = 0, paired = 1, ch;
    while ((ch = getc_unlocked(fp)) != EOF) {
        int ch2 = getc_unlocked(fp);
        if (ch2 == EOF || ch2 != ch) { paired = 0; break; }
        total++;
    }
    funlockfile(fp);
    int count_ok = total == N * PER;

    fclose(fp);
    unlink(path);
    printf("flockfilet paired=%d count=%d\n", paired, count_ok);
    return 0;
}
