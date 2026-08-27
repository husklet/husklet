#include <stdint.h>
#include <string.h>

int main(int argc, char **argv) {
    register uint64_t number __asm__("rax") = argc == 2 && strcmp(argv[1], "group") == 0 ? 231u : 60u;
    register uint64_t status __asm__("rdi") = 0;
    __asm__ volatile("syscall" : "+a"(number) : "D"(status) : "rcx", "r11", "memory");
    __builtin_unreachable();
}
