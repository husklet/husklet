#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

int main(void) {
    const size_t reservation_size = 128u * 1024u * 1024u;
    const size_t mapping_size = 16u * 1024u;
    unsigned char *reservation =
        mmap(NULL, reservation_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return 1;

    void *hint = reservation + 2u * 1024u * 1024u;
    unsigned char *mapping =
        mmap(hint, mapping_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) return 2;

    int relocated = mapping != hint;
    if (munmap(reservation, reservation_size) != 0) return 3;
    mapping[0] = 0x5a;
    int survived = mapping[0] == 0x5a;
    printf("mmap-occupied-hint relocated=%d survived=%d\n", relocated, survived);
    if (munmap(mapping, mapping_size) != 0) return 4;
    return relocated && survived ? 0 : 5;
}
