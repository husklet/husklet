#include "../include/layout.h"

int main(void) {
    return sizeof(hl_native_aarch64_cpu) == 1896 && sizeof(hl_native_x86_64_cpu) == 1560 ? 0 : 1;
}
