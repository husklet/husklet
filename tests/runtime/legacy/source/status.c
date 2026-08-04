#include "abi.h"

void _start(void) {
    static const char message[] = "status\n";
    guest_write(message, sizeof(message) - 1);
    guest_exit(17);
}
