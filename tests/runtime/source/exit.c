#include "abi.h"

void _start(void) {
    guest_exit(42);
}
