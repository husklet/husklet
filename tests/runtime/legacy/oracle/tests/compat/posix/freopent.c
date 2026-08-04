// fdopen wraps a raw fd; freopen redirects an existing stream to a new file; both preserve data.
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>

int main(void) {
    char p1[64], p2[64];
    snprintf(p1, sizeof p1, "/tmp/hl_fr1_%d", (int)getpid());
    snprintf(p2, sizeof p2, "/tmp/hl_fr2_%d", (int)getpid());

    // fdopen: buffered stream over a low-level fd.
    int fd = open(p1, O_CREAT | O_RDWR | O_TRUNC, 0644);
    FILE *f = fdopen(fd, "w+");
    int fdopen_ok = f != NULL;
    if (fdopen_ok) {
        fputs("first", f);
        fflush(f);
        rewind(f);
        char b[8] = {0};
        fdopen_ok = fread(b, 1, 5, f) == 5 && strcmp(b, "first") == 0;
    }

    // freopen: redirect the same FILE* to a second file.
    FILE *g = freopen(p2, "w+", f);
    int freopen_ok = g != NULL;
    if (freopen_ok) {
        fputs("second", g);
        fflush(g);
        rewind(g);
        char b[8] = {0};
        freopen_ok = fread(b, 1, 6, g) == 6 && strcmp(b, "second") == 0;
        fclose(g);
    }

    // p1 keeps its original contents (freopen did not touch it).
    FILE *chk = fopen(p1, "r");
    char b[8] = {0};
    int orig_ok = chk && fread(b, 1, 5, chk) == 5 && strcmp(b, "first") == 0;
    if (chk) fclose(chk);

    unlink(p1);
    unlink(p2);
    printf("freopent fdopen=%d freopen=%d orig=%d\n", fdopen_ok, freopen_ok, orig_ok);
    return 0;
}
