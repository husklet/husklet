#include "../include/layout.h"

int main(void) {
    return sizeof(hl_native_aarch64_cpu) == 2224 && sizeof(hl_native_x86_64_cpu) == 1824 ? 0 : 1;
}
