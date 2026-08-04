// #391 root cause: gpg's keyring_get_keyblock re-opens the keyring, lseek(fd, found.offset, SEEK_SET)s to
// the matched keyblock, then read()s it. Under hl, the lseek did not reposition the file: read() returned
// bytes from offset 0 (the FIRST keyblock) regardless of the seek, so gpg fetched the wrong key -> BADSIG.
// This reproduces exactly that: open a file O_RDONLY, lseek to a mid-file offset, read, and verify the
// bytes match what pread() returns at that offset (i.e. that the seek took effect). Also checks that a
// read after seeking near EOF returns only the remaining bytes, not the whole file.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

static int test_one(const char *path, off_t off) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) { perror("open"); return 1; }
    struct stat st; fstat(fd, &st);
    off_t sz = st.st_size;
    off_t r = lseek(fd, off, SEEK_SET);
    if (r != off) { printf("FAIL %s: lseek returned %lld want %lld\n", path, (long long)r, (long long)off); close(fd); return 1; }
    uint8_t rbuf[4096], pbuf[4096];
    size_t want = (size_t)(sz - off < (off_t)sizeof rbuf ? sz - off : (off_t)sizeof rbuf);
    ssize_t got = read(fd, rbuf, sizeof rbuf);          // read after seek
    ssize_t pgot = pread(fd, pbuf, want, off);          // ground truth at that offset
    int fails = 0;
    if ((size_t)got != want) { printf("FAIL %s off=%lld: read-after-seek got %zd, expected %zu (remaining from offset)\n", path, (long long)off, got, want); fails++; }
    if (got > 0 && pgot > 0 && (got != pgot || memcmp(rbuf, pbuf, got) != 0)) {
        printf("FAIL %s off=%lld: read-after-seek data != pread@offset (seek ignored: reads from 0?) rbuf[0]=%02x pbuf[0]=%02x\n",
               path, (long long)off, rbuf[0], pbuf[0]);
        fails++;
    }
    close(fd);
    return fails;
}

int main(int argc, char **argv) {
    int fails = 0;
    if (argc > 1) {
        // seek to a few mid-file offsets on the given file
        int fd = open(argv[1], O_RDONLY); struct stat st; fstat(fd, &st); off_t sz = st.st_size; close(fd);
        fails += test_one(argv[1], sz / 2);
        fails += test_one(argv[1], sz / 3);
        fails += test_one(argv[1], sz > 100 ? sz - 100 : 0);
    } else {
        // self-contained: create a 3000-byte file with position-dependent content, then seek+read
        char path[] = "/tmp/hllseekXXXXXX";
        int fd = mkstemp(path);
        for (int i = 0; i < 3000; i++) { uint8_t b = (uint8_t)(i * 7 + 3); (void)!write(fd, &b, 1); }
        close(fd);
        for (off_t o = 0; o <= 2900; o += 317) fails += test_one(path, o);
        unlink(path);
    }
    printf("lseek-read %s (fails=%d)\n", fails == 0 ? "OK" : "CORRUPT", fails);
    return fails ? 1 : 0;
}
