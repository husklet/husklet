#ifndef HL_LINUX_ABI_SECCOMP_VM_H
#define HL_LINUX_ABI_SECCOMP_VM_H

#include <stdint.h>

#define HL_LINUX_BPF_MAXINSNS 4096

#define HL_LINUX_SECCOMP_RET_KILL_PROCESS 0x80000000u
#define HL_LINUX_SECCOMP_RET_KILL_THREAD 0x00000000u
#define HL_LINUX_SECCOMP_RET_TRAP 0x00030000u
#define HL_LINUX_SECCOMP_RET_ERRNO 0x00050000u
#define HL_LINUX_SECCOMP_RET_USER_NOTIF 0x7fc00000u
#define HL_LINUX_SECCOMP_RET_TRACE 0x7ff00000u
#define HL_LINUX_SECCOMP_RET_LOG 0x7ffc0000u
#define HL_LINUX_SECCOMP_RET_ALLOW 0x7fff0000u
#define HL_LINUX_SECCOMP_RET_ACTION_FULL 0xffff0000u
#define HL_LINUX_SECCOMP_RET_DATA 0x0000ffffu

struct hl_linux_sock_filter {
    uint16_t code;
    uint8_t jt;
    uint8_t jf;
    uint32_t k;
};

struct hl_linux_seccomp_data {
    int32_t nr;
    uint32_t arch;
    uint64_t instruction_pointer;
    uint64_t args[6];
};

uint32_t hl_seccomp_run(const struct hl_linux_sock_filter *filter, uint16_t length,
                        const struct hl_linux_seccomp_data *data);
int hl_seccomp_precedence(uint32_t action);

#endif
