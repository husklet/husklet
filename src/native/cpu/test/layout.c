#include "../include/layout.h"

int main(void) {
    return sizeof(hl_native_aarch64_cpu) == 2144 && sizeof(hl_native_x86_64_cpu) == 1568 ? 0 : 1;
}
