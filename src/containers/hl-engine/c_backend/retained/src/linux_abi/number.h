#ifndef HL_LINUX_SYSCALL_NUMBER_H
#define HL_LINUX_SYSCALL_NUMBER_H

#include <stdint.h>

#define HL_LINUX_SYSCALL_X86_ONLY UINT64_C(0x10000)

enum hl_linux_guest_isa {
    HL_LINUX_GUEST_AARCH64,
    HL_LINUX_GUEST_X86_64,
};

uint64_t hl_linux_syscall_number(enum hl_linux_guest_isa guest_isa, uint64_t guest_number);
uint64_t hl_linux_syscall_guest_number(enum hl_linux_guest_isa guest_isa, uint64_t canonical_number);

#endif
