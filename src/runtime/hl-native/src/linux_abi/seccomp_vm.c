#include "seccomp_vm.h"

#include <stddef.h>
#include <string.h>

#define CLASS(c) ((c) & 0x07)
#define SIZE(c) ((c) & 0x18)
#define MODE(c) ((c) & 0xe0)
#define OP(c) ((c) & 0xf0)
#define SRC(c) ((c) & 0x08)
#define RVAL(c) ((c) & 0x18)
#define MISCOP(c) ((c) & 0xf8)
#define MEMWORDS 16

uint32_t hl_seccomp_run(const struct hl_linux_sock_filter *f, uint16_t flen, const struct hl_linux_seccomp_data *sd) {
    const uint8_t *packet = (const uint8_t *)sd;
    const uint32_t packet_length = (uint32_t)sizeof(*sd);
    uint32_t accumulator = 0, index = 0, memory[MEMWORDS] = {0}, pc = 0;

    if (!f || !sd || flen == 0 || flen > HL_LINUX_BPF_MAXINSNS) return 0;
    for (uint32_t steps = 0; pc < flen && steps <= HL_LINUX_BPF_MAXINSNS; steps++, pc++) {
        const struct hl_linux_sock_filter *instruction = &f[pc];
        uint16_t code = instruction->code;
        uint32_t value = instruction->k;
        switch (CLASS(code)) {
        case 0x00:
            switch (MODE(code)) {
            case 0x00: accumulator = value; break;
            case 0x80: accumulator = packet_length; break;
            case 0x20:
            case 0x40: {
                uint64_t offset = MODE(code) == 0x40 ? (uint64_t)index + value : value;
                uint32_t size = SIZE(code) == 0x10 ? 1u : (SIZE(code) == 0x08 ? 2u : 4u);
                if (offset + size > packet_length) return 0;
                accumulator = packet[offset];
                if (size > 1) accumulator |= (uint32_t)packet[offset + 1] << 8;
                if (size > 2) accumulator |= (uint32_t)packet[offset + 2] << 16 | (uint32_t)packet[offset + 3] << 24;
                break;
            }
            case 0x60:
                if (value >= MEMWORDS) return 0;
                accumulator = memory[value];
                break;
            default: return 0;
            }
            break;
        case 0x01:
            switch (MODE(code)) {
            case 0x00: index = value; break;
            case 0x80: index = packet_length; break;
            case 0x60:
                if (value >= MEMWORDS) return 0;
                index = memory[value];
                break;
            case 0xa0:
                if (value >= packet_length) return 0;
                index = 4u * (packet[value] & 0x0fu);
                break;
            default: return 0;
            }
            break;
        case 0x02:
            if (value >= MEMWORDS) return 0;
            memory[value] = accumulator;
            break;
        case 0x03:
            if (value >= MEMWORDS) return 0;
            memory[value] = index;
            break;
        case 0x04: {
            uint32_t source = SRC(code) == 0x08 ? index : value;
            switch (OP(code)) {
            case 0x00: accumulator += source; break;
            case 0x10: accumulator -= source; break;
            case 0x20: accumulator *= source; break;
            case 0x30:
                if (!source) return 0;
                accumulator /= source;
                break;
            case 0x40: accumulator |= source; break;
            case 0x50: accumulator &= source; break;
            case 0x60: accumulator = source < 32 ? accumulator << source : 0; break;
            case 0x70: accumulator = source < 32 ? accumulator >> source : 0; break;
            case 0x80: accumulator = 0u - accumulator; break;
            case 0x90:
                if (!source) return 0;
                accumulator %= source;
                break;
            case 0xa0: accumulator ^= source; break;
            default: return 0;
            }
            break;
        }
        case 0x05:
            if (OP(code) == 0x00) {
                if (value >= flen - pc - 1u) return 0;
                pc += value;
            } else {
                uint32_t compare = SRC(code) == 0x08 ? index : value;
                int match;
                switch (OP(code)) {
                case 0x10: match = accumulator == compare; break;
                case 0x20: match = accumulator > compare; break;
                case 0x30: match = accumulator >= compare; break;
                case 0x40: match = (accumulator & compare) != 0; break;
                default: return 0;
                }
                uint32_t jump = match ? instruction->jt : instruction->jf;
                if (jump >= flen - pc - 1u) return 0;
                pc += jump;
            }
            break;
        case 0x06: return RVAL(code) == 0x10 ? accumulator : value;
        case 0x07:
            if (MISCOP(code) == 0x00)
                index = accumulator;
            else if (MISCOP(code) == 0x80)
                accumulator = index;
            else
                return 0;
            break;
        default: return 0;
        }
    }
    return 0;
}

int hl_seccomp_precedence(uint32_t action) {
    switch (action & HL_LINUX_SECCOMP_RET_ACTION_FULL) {
    case HL_LINUX_SECCOMP_RET_KILL_PROCESS: return 0;
    case HL_LINUX_SECCOMP_RET_KILL_THREAD: return 1;
    case HL_LINUX_SECCOMP_RET_TRAP: return 2;
    case HL_LINUX_SECCOMP_RET_ERRNO: return 3;
    case HL_LINUX_SECCOMP_RET_USER_NOTIF: return 4;
    case HL_LINUX_SECCOMP_RET_TRACE: return 5;
    case HL_LINUX_SECCOMP_RET_LOG: return 6;
    case HL_LINUX_SECCOMP_RET_ALLOW: return 7;
    default: return 1;
    }
}
