#include "rotate.h"

#include <stdint.h>

#include "cpu.h"

static uint64_t read_operand(uint64_t address, int width) {
    switch (width) {
    case 1: return *(uint8_t *)address;
    case 2: return *(uint16_t *)address;
    case 4: return *(uint32_t *)address;
    default: return *(uint64_t *)address;
    }
}

void hl_x86_rotate_carry(struct cpu *cpu) {
    uint64_t descriptor = cpu->divop;
    int width = (int)(descriptor & UINT64_C(0xff));
    int rotate_right = (int)((descriptor >> 8) & 1);
    int is_memory = (int)((descriptor >> 9) & 1);
    int high_byte = (int)((descriptor >> 10) & 1);
    int reg = (int)((descriptor >> 16) & UINT64_C(0x1f));
    if ((width != 1 && width != 2 && width != 4 && width != 8) || (!is_memory && reg >= 16) ||
        (high_byte && (is_memory || width != 1)))
        return;
    int bits = 8 * width;
    uint64_t mask = (width == 8) ? UINT64_MAX : ((UINT64_C(1) << bits) - 1);
    uint64_t address = cpu->x87_ea;
    uint64_t value = is_memory ? read_operand(address, width)
                               : (high_byte ? ((cpu->r[reg] >> 8) & UINT64_C(0xff)) : (cpu->r[reg] & mask));
    int count_mask = (width == 8) ? 0x3f : 0x1f;
    int masked = (int)(cpu->r[RCX] & UINT64_C(0xff)) & count_mask;
    int effective = masked % (bits + 1);
    int carry = !((cpu->nzcv >> 29) & 1); // x86 CF carry-in = NOT stored ARM C

    if (effective != 0) {
        uint64_t result;
        int new_carry;
        if (!rotate_right) {
            result = (effective < bits) ? ((value << effective) & mask) : 0;
            result |= (effective == 1) ? (uint64_t)carry : (((uint64_t)carry << (effective - 1)) & mask);
            if (effective >= 2) result |= (value >> (bits + 1 - effective)) & mask;
            new_carry = (int)((value >> (bits - effective)) & 1);
        } else {
            result = (effective < bits) ? (value >> effective) : 0;
            result |= ((uint64_t)carry << (bits - effective)) & mask;
            if (effective >= 2) result |= (value << (bits - effective + 1)) & mask;
            new_carry = (int)((value >> (effective - 1)) & 1);
        }
        carry = new_carry;
        value = result & mask;
        cpu->nzcv = (cpu->nzcv & ~(UINT64_C(1) << 29)) | ((uint64_t)(carry ? 0 : 1) << 29);
        if (masked == 1) {
            int overflow = !rotate_right ? (int)(((value >> (bits - 1)) & 1) ^ (uint64_t)carry)
                                         : (int)(((value >> (bits - 1)) & 1) ^ ((value >> (bits - 2)) & 1));
            cpu->nzcv = (cpu->nzcv & ~(UINT64_C(1) << 28)) | ((uint64_t)overflow << 28);
        }
    }
    if (is_memory) {
        switch (width) {
        case 1: *(uint8_t *)address = (uint8_t)value; break;
        case 2: *(uint16_t *)address = (uint16_t)value; break;
        case 4: *(uint32_t *)address = (uint32_t)value; break;
        default: *(uint64_t *)address = value; break;
        }
    } else if (high_byte)
        cpu->r[reg] = (cpu->r[reg] & ~UINT64_C(0xff00)) | ((value & UINT64_C(0xff)) << 8);
    else if (width == 1)
        cpu->r[reg] = (cpu->r[reg] & ~UINT64_C(0xff)) | (value & UINT64_C(0xff));
    else if (width == 2)
        cpu->r[reg] = (cpu->r[reg] & ~UINT64_C(0xffff)) | (value & UINT64_C(0xffff));
    else
        cpu->r[reg] = value & mask;
}
