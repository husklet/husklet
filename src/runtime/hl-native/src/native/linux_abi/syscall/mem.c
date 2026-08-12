// Linux guest memory syscall dispatch.
//
// The implementation fragments remain in this translation unit because they
// share the engine's private mapping registries. Each fragment owns one Linux
// memory-ABI concern and can be reviewed independently.
#include "memory/access.c"

static int svc_mem(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                   uint64_t a5) {
    switch (nr) {
#include "memory/mapping.c"
#include "memory/map.c"
#include "memory/protection.c"
#include "memory/advice.c"
#include "memory/transfer.c"
#include "memory/barrier.c"
    default: return 0;
    }
    return 1;
}
