/* Windows checkpoint bridge ABI contract. Cross builds link this fixture; a
 * Windows runner may additionally execute it to verify unsupported semantics. */
#include "api.h"

#include <errno.h>
#include <stdint.h>

#if !defined(_WIN32)
#error "this fixture is only for the Windows bridge ABI"
#endif

int main(void) {
    int32_t parent = 7;
    int32_t child = 8;
    if (hl_c_backend_checkpoint_broker_pair(&parent, &child) != HL_STATUS_NOT_SUPPORTED || parent != -1 ||
        child != -1)
        return 1;
    parent = 7;
    if (hl_c_backend_checkpoint_broker_pair(&parent, NULL) != HL_STATUS_INVALID_ARGUMENT || parent != -1) return 7;

    uint64_t host_pid = 9;
    errno = 0;
    if (hl_c_backend_checkpoint_broker_accept(0, 0, &host_pid) != -1 || errno != ENOTSUP || host_pid != 0) return 2;

    int32_t trigger = 10;
    void *mapping = &trigger;
    if (hl_c_backend_checkpoint_trigger_create(&trigger, &mapping) != HL_STATUS_NOT_SUPPORTED || trigger != -1 ||
        mapping != NULL)
        return 3;
    trigger = 10;
    if (hl_c_backend_checkpoint_trigger_create(&trigger, NULL) != HL_STATUS_INVALID_ARGUMENT || trigger != -1) return 8;

    if (hl_c_backend_checkpoint_adopt(2, 0, 0) != HL_STATUS_NOT_SUPPORTED) return 4;
    errno = 0;
    if (hl_c_backend_checkpoint_interrupt_signal(2) != -1 || errno != ENOTSUP) return 5;

    (void)hl_c_backend_checkpoint_trigger_bump(NULL);
    hl_c_backend_checkpoint_trigger_destroy(NULL, -1);
    if (hl_c_backend_checkpoint_configure(NULL, -1, -1) != HL_STATUS_INVALID_ARGUMENT) return 6;
    return 0;
}
