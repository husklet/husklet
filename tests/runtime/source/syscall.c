#include "abi.h"

void _start(void) {
    static const char message[] = "syscall-ok\n";
    long process = guest_call(GUEST_GETPID, 0, 0, 0);
    if (process <= 0) {
        guest_exit(1);
    }
    guest_write(message, sizeof(message) - 1);
    guest_exit(0);
}
