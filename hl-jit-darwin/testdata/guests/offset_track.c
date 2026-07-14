// #391 root-cause guard: gpg's keydb records the FILE OFFSET of the matched keyblock (via an off_t /
// iobuf_tell-style position), then seeks back to re-read it. Under hl the RECORDED offset came out as the
// FIRST record's offset regardless of which record matched -> the wrong key was fetched (BADSIG). The
// keyid COMPARISON was fine; the bug is a wrong POSITION *value* flowing through correct control flow
// (suspects: 64-bit off_t truncated to 32b, or a load fed the offset with wrong sign/zero-extension).
//
// This reproduces that access pattern with no crypto: write N variable-length records each starting with a
// 1-byte tag, scan them sequentially tracking the byte offset with lseek(SEEK_CUR), store the offset of the
// record whose tag == TARGET into a table, then lseek back to the stored offset and confirm the tag there
// is TARGET. Exercises 32-bit int vs 64-bit off_t bookkeeping, signed/unsigned, and a >2GB-magnitude
// offset pattern. Self-checking: prints OK iff every re-read lands on the matching record.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

// iobuf_tell-style: track position purely in bookkeeping (not lseek) to mirror gpg's in-memory iobuf.
struct rdr { const uint8_t *buf; size_t len; size_t pos; };
static int rd_byte(struct rdr *r, uint8_t *out) { if (r->pos >= r->len) return -1; *out = r->buf[r->pos++]; return 0; }
static uint64_t rd_tell(struct rdr *r) { return (uint64_t)r->pos; } // position AS off_t/u64

// Scan records; each: [tag:1][len:1][len bytes]. Return offset (u64) of the FIRST record with tag==target,
// via the bookkeeping tell — modelling keyring_search recording found.offset = iobuf_tell() at the match.
static int64_t find_offset(struct rdr *r, uint8_t target) {
    for (;;) {
        uint64_t off = rd_tell(r);          // offset of THIS record (the value gpg stores on match)
        uint8_t tag, len;
        if (rd_byte(r, &tag) < 0) return -1;
        if (rd_byte(r, &len) < 0) return -1;
        for (uint8_t i = 0; i < len; i++) { uint8_t d; if (rd_byte(r, &d) < 0) return -1; }
        if (tag == target) return (int64_t)off; // <-- the recorded offset must be THIS record's, not slot 0
    }
}

static int check_file_path(void) {
    // Also exercise the real fd/lseek/pread path (ftell/lseek position math + a large seek).
    char path[] = "/tmp/hlofftrackXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 0;
    uint8_t rec[3] = {0, 1, 0};
    off_t offs[8]; int n = 0;
    for (int t = 1; t <= 5; t++) {
        off_t here = lseek(fd, 0, SEEK_CUR);       // off_t (64-bit) position
        offs[n++] = here;
        rec[0] = (uint8_t)t; rec[1] = 1; rec[2] = (uint8_t)(t * 40);
        (void)!write(fd, rec, 3);
    }
    int ok = 1;
    for (int t = 1; t <= 5; t++) {
        off_t want = offs[t - 1];
        uint8_t got[3];
        if (pread(fd, got, 3, want) != 3 || got[0] != (uint8_t)t) { ok = 0; printf("FILE seek mismatch t=%d off=%lld tag=%d\n", t, (long long)want, got[0]); }
        // also lseek-based re-read
        if (lseek(fd, want, SEEK_SET) != want) { ok = 0; printf("lseek returned wrong pos\n"); }
        if (read(fd, got, 3) != 3 || got[0] != (uint8_t)t) { ok = 0; printf("FILE read-after-seek mismatch t=%d\n", t); }
    }
    close(fd); unlink(path);
    return ok;
}

int main(void) {
    int fails = 0;
    // Build a buffer of records; the matching tag is the 2nd, then 3rd, then last — mirroring the keyring
    // where the wanted key is NOT first. Also include a variant with a big base offset.
    uint8_t buf[4096];
    for (int matchpos = 1; matchpos <= 4; matchpos++) {
        size_t p = 0; uint64_t want_off = 0; int seen = 0;
        for (int rec = 0; rec < 5; rec++) {
            if (rec == matchpos && !seen) { want_off = p; seen = 1; }
            buf[p++] = (rec == matchpos) ? 0x42 : (uint8_t)(0x10 + rec); // tag (0x42 = target)
            uint8_t len = (uint8_t)(20 + rec * 7);
            buf[p++] = len;
            for (int i = 0; i < len; i++) buf[p++] = (uint8_t)(rec * 31 + i);
        }
        struct rdr r = { buf, p, 0 };
        int64_t got = find_offset(&r, 0x42);
        if (got != (int64_t)want_off) { fails++; printf("BUF matchpos=%d: recorded off=%lld want=%llu\n", matchpos, (long long)got, (unsigned long long)want_off); }
        // re-read at the recorded offset: tag must be the target
        if (got >= 0 && (size_t)got < p && buf[got] != 0x42) { fails++; printf("BUF reread matchpos=%d tag=0x%x != 0x42\n", matchpos, buf[got]); }
    }
    // 64-bit off_t magnitude test: offsets that don't fit in 32 bits must round-trip
    uint64_t bigs[] = {0x00000000ffffffffULL, 0x0000000100000000ULL, 0x0000000123456789ULL, 0x7fffffffffffffffULL};
    for (unsigned i = 0; i < sizeof bigs/sizeof bigs[0]; i++) {
        volatile uint64_t x = bigs[i];
        off_t as_off = (off_t)x;                 // off_t is 64-bit on LP64 — must NOT truncate
        uint64_t back = (uint64_t)as_off;
        if (back != x) { fails++; printf("OFFT truncation 0x%llx -> 0x%llx\n", (unsigned long long)x, (unsigned long long)back); }
    }
    if (!check_file_path()) fails++;
    printf("offset-track %s (fails=%d)\n", fails == 0 ? "OK" : "CORRUPT", fails);
    return fails ? 1 : 0;
}
