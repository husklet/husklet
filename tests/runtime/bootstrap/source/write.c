#include "abi.h"

void _start(void) {
    static const char message[] = "compat-write\n";
    long written = guest_write(message, sizeof(message) - 1);
    guest_exit(written == (long)(sizeof(message) - 1) ? 0 : 1);
}
