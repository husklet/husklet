// A file-backed page mapped immediately after a private anonymous one, driven
// from a hot native body that straddles the boundary. The two pages have
// different host backings -- the private page lives in the flat arena, the file
// page in its own sparse host mapping at its own delta -- so a view of the
// private page that reaches one page too far resolves the file page through the
// arena and neither the stores nor the loads ever reach the file.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static volatile unsigned long offset;
static volatile unsigned long sink;

// Straight-line bodies never reach native admission; this counted walk over a
// whole page is what gets the guest translated at all, and every later probe
// depends on it having run first.
static void warm(volatile unsigned char *base, unsigned long limit, unsigned rounds) {
    for (unsigned round = 0; round < rounds; round++) {
        unsigned long total = 0;
        for (offset = 0; offset < limit; offset++) {
            base[offset] = (unsigned char)(offset & 0xff);
            total += base[offset];
        }
        sink = total;
    }
}

static int prepare(const char *path, unsigned long page, unsigned char fill) {
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    unsigned char *buffer;
    if (fd < 0) return -1;
    buffer = malloc(page);
    if (buffer == NULL) return -1;
    memset(buffer, fill, page);
    if (write(fd, buffer, page) != (long)page) return -1;
    free(buffer);
    return fd;
}

// Maps `page` bytes of `fd` immediately after one private anonymous page.
static unsigned char *adjacent(int fd, unsigned long page) {
    unsigned char *base = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) return NULL;
    memset(base, 0x11, page);
    if (mmap(base + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, 0) != base + page) return NULL;
    return base;
}

static int store_probe(unsigned long page) {
    int fd = prepare("/tmp/hl-view-sparse-store", page, 0xaa);
    unsigned char check[8];
    unsigned char *base;
    if (fd < 0 || (base = adjacent(fd, page)) == NULL) {
        printf("store setup failed\n");
        return 1;
    }
    warm(base, page, 24);
    // The private page keeps the body warm while the tail of each pass lands in
    // the file page, so one translated loop reaches both backings.
    for (unsigned round = 0; round < 24; round++) {
        for (offset = page - 8; offset < page + 8; offset++) {
            base[offset] = (unsigned char)(0x70 + (offset & 7));
        }
    }
    // Read the object back through the descriptor rather than the mapping, so
    // the answer comes from the file and not from what the loop left behind.
    if (pread(fd, check, sizeof check, 0) != (long)sizeof check) {
        printf("store readback failed\n");
        return 1;
    }
    printf("store file %02x %02x %02x %02x\n", check[0], check[1], check[2], check[3]);
    printf("store private %02x %02x\n", base[page - 1], base[page - 8]);
    return 0;
}

static int load_probe(unsigned long page) {
    int fd = prepare("/tmp/hl-view-sparse-load", page, 0xc3);
    unsigned char *base;
    unsigned long seen = 0;
    if (fd < 0 || (base = adjacent(fd, page)) == NULL) {
        printf("load setup failed\n");
        return 1;
    }
    warm(base, page, 24);
    for (unsigned round = 0; round < 24; round++) {
        unsigned long total = 0;
        for (offset = page - 8; offset < page + 8; offset++) {
            total += base[offset];
        }
        seen = total;
    }
    // Eight private bytes left by the warm walk and eight 0xc3 file bytes.
    printf("load sum %lu\n", seen);
    return 0;
}

int main(void) {
    unsigned long page = (unsigned long)sysconf(_SC_PAGESIZE);
    if (store_probe(page) != 0) {
        return 1;
    }
    return load_probe(page);
}
