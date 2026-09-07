#ifndef HL_TRANSLATOR_OWNER_H
#define HL_TRANSLATOR_OWNER_H

#include <stdint.h>

/* Persisted owner-preserve ABI shared by the generic live ledger and ISA validators. */
#define JIT_BODY_OWNER_PRESERVE_RET_RAX (UINT32_C(1) << 16)
#define JIT_BODY_OWNER_FLAGS_FROM_CPU (UINT32_C(1) << 17)
#define JIT_BODY_OWNER_FLAGS_FROM_PACKED (UINT32_C(1) << 18)
_Static_assert((JIT_BODY_OWNER_PRESERVE_RET_RAX & UINT16_MAX) == 0 &&
                   (JIT_BODY_OWNER_FLAGS_FROM_CPU &
                    (UINT16_MAX | JIT_BODY_OWNER_PRESERVE_RET_RAX)) == 0 &&
                   (JIT_BODY_OWNER_FLAGS_FROM_PACKED &
                    (UINT16_MAX | JIT_BODY_OWNER_PRESERVE_RET_RAX |
                     JIT_BODY_OWNER_FLAGS_FROM_CPU)) == 0,
               "body owner metadata must not collide with the GPR preserve mask");

#endif
