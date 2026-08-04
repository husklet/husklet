#include <stdio.h>
#include <sys/mman.h>
#include <string.h>

int main(void) {
    size_t size = 1u << 20;
    char *memory = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    long sum = 0;
    if (memory == MAP_FAILED) return 1;
    memset(memory, 3, size);
    for (size_t offset = 0; offset < size; offset += 4096) sum += memory[offset];
    if (munmap(memory, size) != 0) return 1;
    printf("mmap sum=%ld\n", sum);
    return sum == 768 ? 0 : 1;
}
