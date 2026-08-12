#include "hl/engine.h"

/* Production supplies these three process-lifecycle callbacks from
 * core/lifecycle.c.  This smoke owns only the backend/link boundary, so inert
 * definitions close that documented boundary without pulling in a second
 * production entry point. */
void hl_engine_child_result_publish(int32_t guest_status, hl_status engine_status, uint64_t detail) {
    (void)guest_status;
    (void)engine_status;
    (void)detail;
}

void hl_engine_child_result_publish_signal(int32_t guest_signal) {
    (void)guest_signal;
}

void hl_engine_child_result_after_fork(void) {
}

/* The smoke link pulls the non-selected interpreter unity object and its
 * runtime archive into one executable. Reaching main proves the imported
 * object has no unresolved retained-engine dependency. It deliberately does
 * not launch a guest or alter production selection. */
int main(void) {
    return 0;
}
